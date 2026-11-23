//! The projection unit writer: the one atomic persist step of a market's
//! canonical deltas and its exact cursor (ADR-105 D8).
//!
//! Gated with the write substrate (ADR-070): a reader build compiles none of it.
//! Every refusal leaves the caller's transaction uncommitted, so the unit's
//! journal rows, chain identities, generation relationships, and cursor advance
//! move together or not at all.
//!
//! The statements are proportional to the unit's DISTINCT identities, not to its
//! deltas: one multi-row statement per batch of 100 distinct blocks,
//! transactions, generations, and journal entries.

use alloy::primitives::B256;
use degenbot_core::canonical_head::ChainAnchor;
use rusqlite::{params, Connection, OptionalExtension};

use super::{
    encode_hash, read_projection_cursor_on_conn, CursorAnchor, DeltaJournalEntry,
    GenerationActivation, GenerationRecord, LogCoordinate, ProjectionCursor, ProjectionError,
    ProjectionUnit,
};

/// The market row's numeric claim: its chain and the `last_update_block` the
/// exact cursor replaces.
fn read_market_claim(
    conn: &Connection,
    market_id: i64,
) -> Result<(i64, Option<u64>), ProjectionError> {
    let row: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT chain_id, last_update_block FROM aave_v3_markets WHERE id = ?1",
            params![market_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (chain_id, numeric) = row.ok_or(ProjectionError::MissingMarket(market_id))?;
    let numeric = numeric
        .map(|numeric| {
            u64::try_from(numeric)
                .map_err(|_| ProjectionError::Decode(format!("last_update_block {numeric}")))
        })
        .transpose()?;
    Ok((chain_id, numeric))
}

fn chain_id_column(chain_id: u64) -> Result<i64, ProjectionError> {
    i64::try_from(chain_id).map_err(|_| ProjectionError::Decode(format!("chain id {chain_id}")))
}

fn index_column(value: u64, what: &str) -> Result<i64, ProjectionError> {
    i64::try_from(value).map_err(|_| ProjectionError::Decode(format!("{what} {value}")))
}

/// One unit's distinct chain identities and generation relationships, in
/// deterministic order so the statements a unit runs are byte-stable.
struct UnitIdentities {
    blocks: Vec<ChainAnchor>,
    transactions: Vec<LogCoordinate>,
    generations: Vec<GenerationRecord>,
}

/// The per-entry ordinal of each delta among the deltas emitted at its exact
/// chain position (ADR-105 D8's branch-position uniqueness: one log may carry
/// several deltas, and the k-th delta at a position is unique within a market).
fn position_ordinals(entries: &[DeltaJournalEntry]) -> Vec<i64> {
    let mut seen: std::collections::HashMap<(B256, u64), i64> = std::collections::HashMap::new();
    let mut ordinals = Vec::with_capacity(entries.len());
    for entry in entries {
        let ordinal = {
            let slot = seen
                .entry((entry.position.tx_hash(), entry.position.log_index()))
                .or_insert(0);
            *slot += 1;
            *slot
        };
        ordinals.push(ordinal);
    }
    ordinals
}

/// Every distinct identity and generation relationship the unit names, deduped
/// and ordered so one unit writes each of them once.
///
/// An event-activated generation's own position is a chain identity too, so it
/// is recorded even when the delta that consumed it sits in a later block than
/// any journaled entry.
fn collect_identities(unit: &ProjectionUnit<'_>) -> UnitIdentities {
    let mut blocks: std::collections::BTreeMap<B256, ChainAnchor> =
        std::collections::BTreeMap::new();
    let mut transactions: std::collections::BTreeMap<B256, LogCoordinate> =
        std::collections::BTreeMap::new();
    let mut generations: std::collections::BTreeMap<String, GenerationRecord> =
        std::collections::BTreeMap::new();
    for entry in unit.entries {
        blocks.insert(entry.position.anchor().hash(), entry.position.anchor());
        transactions.insert(entry.position.tx_hash(), entry.position);
        if let Some(generation) = &entry.generation {
            if let GenerationActivation::Event(at) = generation.activation {
                blocks.insert(at.anchor().hash(), at.anchor());
                transactions.insert(at.tx_hash(), at);
            }
            generations.insert(generation_key(generation), generation.clone());
        }
    }
    UnitIdentities {
        blocks: blocks.into_values().collect(),
        transactions: transactions.into_values().collect(),
        generations: generations.into_values().collect(),
    }
}

/// The numbered placeholder list a multi-value IN (...) predicate binds from
/// `first` onward.
fn in_list(first: usize, count: usize) -> String {
    (first..first + count)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The identity a generation relationship is deduped and looked up by: the
/// entity key plus its activation (a seed generation is identified by its
/// revision; an event generation by the event that activated it).
fn generation_key(generation: &GenerationRecord) -> String {
    let revision = generation.revision.map(i64::from);
    match generation.activation {
        GenerationActivation::Seed => seed_key(&generation.entity, revision),
        GenerationActivation::Event(at) => event_key(
            &generation.entity,
            i64::try_from(at.anchor().chain_id()).unwrap_or(0),
            &encode_hash(at.tx_hash()),
            i64::try_from(at.log_index()).unwrap_or(0),
            revision,
        ),
    }
}

/// The identity of a resolution-start generation: one per entity revision.
fn seed_key(entity: &str, revision: Option<i64>) -> String {
    format!("seed|{entity}|{revision:?}")
}

/// The identity of an event-activated generation: one per entity activation.
fn event_key(
    entity: &str,
    chain_id: i64,
    tx_hash: &str,
    log_index: i64,
    revision: Option<i64>,
) -> String {
    format!("event|{entity}|{chain_id}|{tx_hash}|{log_index}|{revision:?}")
}

/// Rows per multi-row statement: 100 x 13 columns stays far below `SQLite`'s
/// bound-variable ceiling, and one statement per 100 rows keeps a chunk's write
/// surface proportional to its distinct identities rather than to its deltas.
const IDENTITY_BATCH_ROWS: usize = 100;

fn placeholders(rows: usize, columns: usize) -> String {
    let mut sql = String::new();
    for row in 0..rows {
        if row > 0 {
            sql.push_str(", ");
        }
        sql.push('(');
        for column in 0..columns {
            if column > 0 {
                sql.push_str(", ");
            }
            let index = row * columns + column + 1;
            sql.push('?');
            sql.push_str(&index.to_string());
        }
        sql.push(')');
    }
    sql
}

/// One batched `INSERT OR IGNORE` per [`IDENTITY_BATCH_ROWS`] rows.
fn insert_rows(
    conn: &Connection,
    into: &str,
    columns: usize,
    rows: &[Vec<rusqlite::types::Value>],
) -> Result<(), ProjectionError> {
    for batch in rows.chunks(IDENTITY_BATCH_ROWS) {
        let sql = format!(
            "INSERT OR IGNORE {into} VALUES {}",
            placeholders(batch.len(), columns)
        );
        let values: Vec<rusqlite::types::Value> =
            batch.iter().flat_map(|row| row.iter().cloned()).collect();
        conn.execute(&sql, rusqlite::params_from_iter(values))?;
    }
    Ok(())
}

/// Record the unit's block identities, refusing a hash already recorded at
/// another height or with another parent (a block hash names one block).
fn write_block_identities(
    conn: &Connection,
    blocks: &[ChainAnchor],
) -> Result<(), ProjectionError> {
    let mut rows = Vec::with_capacity(blocks.len());
    for anchor in blocks {
        rows.push(vec![
            rusqlite::types::Value::Integer(chain_id_column(anchor.chain_id())?),
            rusqlite::types::Value::Integer(index_column(anchor.number(), "block number")?),
            rusqlite::types::Value::Text(encode_hash(anchor.hash())),
            rusqlite::types::Value::Text(encode_hash(anchor.parent_hash())),
        ]);
    }
    insert_rows(
        conn,
        "INTO aave_v3_block_anchors (chain_id, block_number, block_hash, parent_hash)",
        4,
        &rows,
    )?;
    for batch in blocks.chunks(IDENTITY_BATCH_ROWS) {
        let chain_id = chain_id_column(batch.first().map_or(1, ChainAnchor::chain_id))?;
        let hashes: Vec<rusqlite::types::Value> = batch
            .iter()
            .map(|anchor| rusqlite::types::Value::Text(encode_hash(anchor.hash())))
            .collect();
        let sql = format!(
            "SELECT block_hash, block_number, parent_hash FROM aave_v3_block_anchors \
             WHERE chain_id = ?1 AND block_hash IN ({})",
            in_list(2, batch.len())
        );
        let mut parameters: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Integer(chain_id)];
        parameters.extend(hashes.clone());
        let mut statement = conn.prepare(&sql)?;
        let mut found: std::collections::HashMap<String, (i64, String)> =
            std::collections::HashMap::new();
        let mut cursor = statement.query(rusqlite::params_from_iter(parameters))?;
        while let Some(row) = cursor.next()? {
            found.insert(row.get(0)?, (row.get(1)?, row.get(2)?));
        }
        drop(cursor);
        drop(statement);
        for anchor in batch {
            let hash = encode_hash(anchor.hash());
            let number = index_column(anchor.number(), "block number")?;
            let parent = encode_hash(anchor.parent_hash());
            match found.get(&hash) {
                Some((recorded_number, recorded_parent))
                    if *recorded_number == number && *recorded_parent == parent => {}
                _ => {
                    return Err(ProjectionError::BlockIdentityConflict {
                        block_hash: anchor.hash(),
                        number: anchor.number(),
                    })
                }
            }
        }
    }
    Ok(())
}

/// Record the unit's transaction identities, refusing a hash already recorded at
/// another block or transaction index.
fn write_transaction_identities(
    conn: &Connection,
    transactions: &[LogCoordinate],
) -> Result<(), ProjectionError> {
    let mut rows = Vec::with_capacity(transactions.len());
    for position in transactions {
        rows.push(vec![
            rusqlite::types::Value::Integer(chain_id_column(position.anchor().chain_id())?),
            rusqlite::types::Value::Text(encode_hash(position.tx_hash())),
            rusqlite::types::Value::Text(encode_hash(position.anchor().hash())),
            rusqlite::types::Value::Integer(index_column(position.tx_index(), "tx index")?),
        ]);
    }
    insert_rows(
        conn,
        "INTO aave_v3_transaction_anchors (chain_id, tx_hash, block_hash, tx_index)",
        4,
        &rows,
    )?;
    for batch in transactions.chunks(IDENTITY_BATCH_ROWS) {
        let chain_id = chain_id_column(
            batch
                .first()
                .map_or(1, |position| position.anchor().chain_id()),
        )?;
        let hashes: Vec<rusqlite::types::Value> = batch
            .iter()
            .map(|position| rusqlite::types::Value::Text(encode_hash(position.tx_hash())))
            .collect();
        let sql = format!(
            "SELECT tx_hash, block_hash, tx_index FROM aave_v3_transaction_anchors \
             WHERE chain_id = ?1 AND tx_hash IN ({})",
            in_list(2, batch.len())
        );
        let mut parameters: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Integer(chain_id)];
        parameters.extend(hashes.clone());
        let mut statement = conn.prepare(&sql)?;
        let mut found: std::collections::HashMap<String, (String, i64)> =
            std::collections::HashMap::new();
        let mut cursor = statement.query(rusqlite::params_from_iter(parameters))?;
        while let Some(row) = cursor.next()? {
            found.insert(row.get(0)?, (row.get(1)?, row.get(2)?));
        }
        drop(cursor);
        drop(statement);
        for position in batch {
            let hash = encode_hash(position.tx_hash());
            let block_hash = encode_hash(position.anchor().hash());
            let tx_index = index_column(position.tx_index(), "tx index")?;
            match found.get(&hash) {
                Some((recorded_block, recorded_index))
                    if *recorded_block == block_hash && *recorded_index == tx_index => {}
                _ => {
                    return Err(ProjectionError::TransactionIdentityConflict {
                        tx_hash: position.tx_hash(),
                        block_hash: position.anchor().hash(),
                        tx_index: position.tx_index(),
                    })
                }
            }
        }
    }
    Ok(())
}

/// Check every generation relationship the unit names: an event-activated
/// generation must be READABLE at the delta that consumed it — same chain, and
/// at or before the delta's own position on the same branch (ADR-105 D6: a
/// replacement creates a new generation at its exact event position rather than
/// rewriting what an earlier event meant).
fn validate_generations(unit: &ProjectionUnit<'_>) -> Result<(), ProjectionError> {
    for entry in unit.entries {
        let Some(generation) = &entry.generation else {
            continue;
        };
        if generation.entity.is_empty() {
            return Err(ProjectionError::InvalidGenerationEntity {
                market_id: unit.market_id,
            });
        }
        let GenerationActivation::Event(at) = generation.activation else {
            continue;
        };
        let readable = at.anchor().chain_id() == entry.position.anchor().chain_id()
            && at.anchor().number() <= entry.position.anchor().number()
            && (at.anchor().number() < entry.position.anchor().number()
                || (at.anchor().hash() == entry.position.anchor().hash()
                    && (at.tx_index(), at.log_index())
                        <= (entry.position.tx_index(), entry.position.log_index())));
        if !readable {
            return Err(ProjectionError::GenerationNotForward {
                market_id: unit.market_id,
                entity: generation.entity.clone(),
            });
        }
    }
    Ok(())
}

/// Record the unit's generation relationships once each, returning the row id
/// every one of them resolved to.
///
/// The lookup reads the market's generation rows once (the table is bounded by
/// the market's entities, not by its deltas), so naming the same generation
/// from thousands of deltas costs one statement per batch of distinct
/// generations rather than one per delta.
fn write_generations(
    conn: &Connection,
    unit: &ProjectionUnit<'_>,
    generations: &[GenerationRecord],
) -> Result<std::collections::HashMap<String, i64>, ProjectionError> {
    let mut ids = std::collections::HashMap::new();
    if generations.is_empty() {
        return Ok(ids);
    }
    let mut rows = Vec::with_capacity(generations.len());
    for generation in generations {
        let (kind, chain, number, block_hash, parent_hash, tx_hash, tx_index, log_index) =
            match generation.activation {
                GenerationActivation::Seed => ("seed", None, None, None, None, None, None, None),
                GenerationActivation::Event(at) => (
                    "event",
                    Some(chain_id_column(at.anchor().chain_id())?),
                    Some(index_column(at.anchor().number(), "block number")?),
                    Some(encode_hash(at.anchor().hash())),
                    Some(encode_hash(at.anchor().parent_hash())),
                    Some(encode_hash(at.tx_hash())),
                    Some(index_column(at.tx_index(), "tx index")?),
                    Some(index_column(at.log_index(), "log index")?),
                ),
            };
        rows.push(vec![
            rusqlite::types::Value::Integer(unit.market_id),
            rusqlite::types::Value::Text(generation.entity.clone()),
            rusqlite::types::Value::Text(kind.to_string()),
            generation
                .revision
                .map_or(rusqlite::types::Value::Null, |revision| {
                    rusqlite::types::Value::Integer(i64::from(revision))
                }),
            chain.map_or(
                rusqlite::types::Value::Null,
                rusqlite::types::Value::Integer,
            ),
            number.map_or(
                rusqlite::types::Value::Null,
                rusqlite::types::Value::Integer,
            ),
            block_hash.map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Text),
            parent_hash.map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Text),
            tx_hash.map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Text),
            tx_index.map_or(
                rusqlite::types::Value::Null,
                rusqlite::types::Value::Integer,
            ),
            log_index.map_or(
                rusqlite::types::Value::Null,
                rusqlite::types::Value::Integer,
            ),
        ]);
    }
    insert_rows(
        conn,
        "INTO aave_v3_generations (market_id, entity, activation_kind, revision, \
         chain_id, block_number, block_hash, parent_hash, tx_hash, tx_index, log_index)",
        11,
        &rows,
    )?;
    let mut statement = conn.prepare(
        "SELECT id, entity, activation_kind, revision, chain_id, tx_hash, log_index \
         FROM aave_v3_generations WHERE market_id = ?1",
    )?;
    let mut known: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut cursor = statement.query(params![unit.market_id])?;
    while let Some(row) = cursor.next()? {
        let entity: String = row.get(1)?;
        let kind: String = row.get(2)?;
        let revision: Option<i64> = row.get(3)?;
        let chain: Option<i64> = row.get(4)?;
        let tx_hash: Option<String> = row.get(5)?;
        let log_index: Option<i64> = row.get(6)?;
        let key = if kind == "seed" {
            seed_key(&entity, revision)
        } else {
            event_key(
                &entity,
                chain.unwrap_or(0),
                &tx_hash.unwrap_or_default(),
                log_index.unwrap_or(0),
                revision,
            )
        };
        known.insert(key, row.get(0)?);
    }
    drop(cursor);
    drop(statement);
    for generation in generations {
        let key = generation_key(generation);
        let id = known
            .get(&key)
            .copied()
            .ok_or_else(|| ProjectionError::MissingGeneration {
                market_id: unit.market_id,
                entity: generation.entity.clone(),
            })?;
        ids.insert(key, id);
    }
    Ok(ids)
}

/// Journal the unit's deltas in emission order, one multi-row statement per
/// [`IDENTITY_BATCH_ROWS`] entries.
///
/// The branch-position uniqueness is the table's
/// `UNIQUE (market_id, chain_id, tx_hash, log_index, position_ordinal)`: a
/// replayed or duplicated position is refused by the constraint, and the
/// refusal is attributed to its entry on the error path only.
fn write_journal_entries(
    conn: &Connection,
    unit: &ProjectionUnit<'_>,
    ordinals: &[i64],
    sequence_start: i64,
    generation_ids: &std::collections::HashMap<String, i64>,
) -> Result<(), ProjectionError> {
    let mut rows = Vec::with_capacity(unit.entries.len());
    for (index, entry) in unit.entries.iter().enumerate() {
        let sequence = sequence_start.saturating_add(i64::try_from(index + 1).unwrap_or(i64::MAX));
        let generation_id = match &entry.generation {
            None => rusqlite::types::Value::Null,
            Some(generation) => {
                let id = generation_ids
                    .get(&generation_key(generation))
                    .copied()
                    .ok_or_else(|| ProjectionError::MissingGeneration {
                        market_id: unit.market_id,
                        entity: generation.entity.clone(),
                    })?;
                rusqlite::types::Value::Integer(id)
            }
        };
        rows.push(vec![
            rusqlite::types::Value::Integer(unit.market_id),
            rusqlite::types::Value::Integer(sequence),
            rusqlite::types::Value::Integer(ordinals.get(index).copied().unwrap_or(1)),
            rusqlite::types::Value::Integer(chain_id_column(entry.position.anchor().chain_id())?),
            rusqlite::types::Value::Integer(index_column(
                entry.position.anchor().number(),
                "block number",
            )?),
            rusqlite::types::Value::Text(encode_hash(entry.position.anchor().hash())),
            rusqlite::types::Value::Text(encode_hash(entry.position.anchor().parent_hash())),
            rusqlite::types::Value::Text(encode_hash(entry.position.tx_hash())),
            rusqlite::types::Value::Integer(index_column(entry.position.tx_index(), "tx index")?),
            rusqlite::types::Value::Integer(index_column(entry.position.log_index(), "log index")?),
            generation_id,
            rusqlite::types::Value::Text(unit.encoding_version.to_string()),
            rusqlite::types::Value::Text(entry.encoding.clone()),
        ]);
    }
    for batch in rows.chunks(IDENTITY_BATCH_ROWS) {
        let sql = format!(
            "INSERT INTO aave_v3_delta_journal \
                (market_id, sequence, position_ordinal, chain_id, block_number, block_hash, \
                 parent_hash, tx_hash, tx_index, log_index, generation_id, encoding_version, \
                 delta_encoding) VALUES {}",
            placeholders(batch.len(), 13)
        );
        let values: Vec<rusqlite::types::Value> =
            batch.iter().flat_map(|row| row.iter().cloned()).collect();
        if let Err(error) = conn.execute(&sql, rusqlite::params_from_iter(values)) {
            return Err(attribute_journal_conflict(conn, unit, ordinals, error));
        }
    }
    Ok(())
}

/// Attribute a journal write's constraint failure: a taken position ordinal is
/// the duplicate branch position; anything else stays a `SQLite` failure.
fn attribute_journal_conflict(
    conn: &Connection,
    unit: &ProjectionUnit<'_>,
    ordinals: &[i64],
    error: rusqlite::Error,
) -> ProjectionError {
    let unique = matches!(&error, rusqlite::Error::SqliteFailure(failure, _)
        if failure.extended_code == 2067 || failure.extended_code == 1555);
    if unique {
        for (index, entry) in unit.entries.iter().enumerate() {
            let taken: bool = match conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM aave_v3_delta_journal WHERE market_id = ?1 \
                 AND chain_id = ?2 AND tx_hash = ?3 AND log_index = ?4 AND position_ordinal = ?5)",
                params![
                    unit.market_id,
                    chain_id_column(entry.position.anchor().chain_id()).unwrap_or_default(),
                    encode_hash(entry.position.tx_hash()),
                    index_column(entry.position.log_index(), "log index").unwrap_or_default(),
                    ordinals.get(index).copied().unwrap_or(1),
                ],
                |row| row.get(0),
            ) {
                Ok(taken) => taken,
                Err(probe) => return ProjectionError::Sqlite(probe),
            };
            if taken {
                return ProjectionError::DuplicateBranchPosition {
                    market_id: unit.market_id,
                    tx_hash: entry.position.tx_hash(),
                    log_index: entry.position.log_index(),
                    position_ordinal: ordinals.get(index).copied().unwrap_or(1),
                };
            }
        }
    }
    ProjectionError::Sqlite(error)
}
/// Persist one projection unit: its journal entries, the chain identities they
/// name, their generation relationships, and the cursor advance — all inside
/// the caller's transaction (ADR-105 D8).
///
/// The unit is checked against its [`CursorAnchor`] token BEFORE anything is
/// written (a refusal there writes nothing at all), and the cursor advance
/// itself is the LAST write: a refusal there leaves the unit's journal rows in
/// the caller's transaction, and the caller's rollback removes them — journal
/// and cursor move together or not at all.
///
/// Returns the market's cursor after the unit: `None` when the market still has
/// no exact anchor (a unit that journals nothing cannot establish one).
///
/// # Errors
///
/// Every [`ProjectionError`] refusal leaves the caller's transaction
/// uncommitted, so the unit is atomic. See [`CursorAnchor`] for the token each
/// advance is checked against.
///
/// # Panics
///
/// Never: every conversion is fallible and refused explicitly.
#[expect(clippy::too_many_lines)]
#[cfg(any(test, feature = "writer"))]
pub fn write_projection_unit_on_conn(
    conn: &Connection,
    unit: &ProjectionUnit<'_>,
) -> Result<Option<ProjectionCursor>, ProjectionError> {
    let (market_chain, numeric_cursor) = read_market_claim(conn, unit.market_id)?;
    let chain = u64::try_from(market_chain).map_err(|_| ProjectionError::InvalidMarketChain {
        market_id: unit.market_id,
        chain_id: market_chain,
    })?;
    if chain != u64::try_from(unit.chain_id).unwrap_or(u64::MAX) {
        return Err(ProjectionError::ChainMismatch {
            market_id: unit.market_id,
            market: chain,
            entry: u64::try_from(unit.chain_id).unwrap_or(u64::MAX),
        });
    }

    let held = read_projection_cursor_on_conn(conn, unit.market_id)?;
    let numeric_claim = match unit.anchor {
        CursorAnchor::First {
            replaces_numeric_cursor,
        } => Some(replaces_numeric_cursor),
        CursorAnchor::Unchanged | CursorAnchor::At(_) => None,
    };

    for entry in unit.entries {
        if entry.position.anchor().chain_id() != chain {
            return Err(ProjectionError::ChainMismatch {
                market_id: unit.market_id,
                market: chain,
                entry: entry.position.anchor().chain_id(),
            });
        }
        if entry.encoding.is_empty() || unit.encoding_version.is_empty() {
            return Err(ProjectionError::Decode(
                "a journal entry must carry its encoding and version".to_string(),
            ));
        }
    }

    let ordinals = position_ordinals(unit.entries);

    match (&unit.anchor, &held) {
        (CursorAnchor::Unchanged, _) => {
            if !unit.entries.is_empty() {
                return Err(ProjectionError::AnchorRequired {
                    market_id: unit.market_id,
                });
            }
        }
        (
            CursorAnchor::First {
                replaces_numeric_cursor,
            },
            None,
        ) => {
            if unit.entries.is_empty() {
                return Err(ProjectionError::FirstAnchorWithoutEvidence {
                    market_id: unit.market_id,
                });
            }
            if numeric_cursor != *replaces_numeric_cursor {
                return Err(ProjectionError::FirstAnchorUnproven {
                    market_id: unit.market_id,
                    named: *replaces_numeric_cursor,
                    held: numeric_cursor,
                });
            }
            if let (Some(replaces), Some(first)) = (replaces_numeric_cursor, unit.entries.first()) {
                if first.position.anchor().number() <= *replaces {
                    return Err(ProjectionError::FirstAnchorNotForward {
                        market_id: unit.market_id,
                        replaces: *replaces,
                        arrived: first.position.anchor().number(),
                    });
                }
            }
        }
        (CursorAnchor::First { .. }, Some(_)) => {
            return Err(ProjectionError::CursorAlreadyEstablished {
                market_id: unit.market_id,
            });
        }
        (CursorAnchor::At(_), None) => {
            return Err(ProjectionError::CursorMissing {
                market_id: unit.market_id,
            });
        }
        (CursorAnchor::At(expected), Some(current)) => {
            if expected != current {
                return Err(ProjectionError::UnexpectedParent {
                    market_id: unit.market_id,
                    expected: Box::new(*expected),
                    held: Box::new(*current),
                });
            }
        }
    }

    if let (Some(current), Some(first)) = (&held, unit.entries.first()) {
        let held_position = current.position;
        let arrived = first.position;
        if arrived.anchor().number() == held_position.anchor().number() {
            if arrived.anchor().hash() != held_position.anchor().hash() {
                return Err(ProjectionError::BranchMismatch {
                    market_id: unit.market_id,
                    number: arrived.anchor().number(),
                });
            }
            if !arrived.is_forward_of(&held_position) {
                return Err(ProjectionError::NotForward {
                    market_id: unit.market_id,
                    held: held_position.anchor().number(),
                    arrived: arrived.anchor().number(),
                });
            }
        } else if arrived.anchor().number() < held_position.anchor().number() {
            return Err(ProjectionError::NotForward {
                market_id: unit.market_id,
                held: held_position.anchor().number(),
                arrived: arrived.anchor().number(),
            });
        } else if arrived.anchor().number() == held_position.anchor().number() + 1
            && arrived.anchor().parent_hash() != held_position.anchor().hash()
        {
            return Err(ProjectionError::BrokenParentLink {
                market_id: unit.market_id,
                number: arrived.anchor().number(),
                parent: arrived.anchor().parent_hash(),
                expected: held_position.anchor().hash(),
            });
        }
    }

    for pair in unit.entries.windows(2) {
        let (previous, next) = (pair[0].position, pair[1].position);
        if next.contradicts_branch_of(&previous)
            || next.anchor().number() < previous.anchor().number()
        {
            return Err(ProjectionError::BranchMismatch {
                market_id: unit.market_id,
                number: next.anchor().number(),
            });
        }
        if next.anchor().number() == previous.anchor().number()
            && next.tx_index() < previous.tx_index()
        {
            return Err(ProjectionError::NotForward {
                market_id: unit.market_id,
                held: previous.anchor().number(),
                arrived: next.anchor().number(),
            });
        }
    }

    validate_generations(unit)?;
    let sequence_start = held.map_or(0, |cursor| cursor.journal_sequence);
    let mut sequence = sequence_start;
    let identities = collect_identities(unit);
    write_block_identities(conn, &identities.blocks)?;
    write_transaction_identities(conn, &identities.transactions)?;
    let generation_ids = write_generations(conn, unit, &identities.generations)?;
    write_journal_entries(conn, unit, &ordinals, sequence_start, &generation_ids)?;
    sequence += i64::try_from(unit.entries.len()).unwrap_or(i64::MAX);

    let Some(last) = unit.entries.last().map(|entry| entry.position) else {
        let Some(current) = held else {
            return Ok(None);
        };
        require_range_forward(unit, current.range_end_block)?;
        let advanced = conn.execute(
            "UPDATE aave_v3_projection_cursors SET range_end_block = ?2 \
             WHERE market_id = ?1 AND block_hash = ?3 AND tx_hash = ?4 \
             AND log_index = ?5 AND journal_sequence = ?6",
            params![
                unit.market_id,
                index_column(unit.range_end_block, "range end")?,
                encode_hash(current.position.anchor().hash()),
                encode_hash(current.position.tx_hash()),
                index_column(current.position.log_index(), "log index")?,
                current.journal_sequence,
            ],
        )?;
        if advanced != 1 {
            return Err(ProjectionError::UnexpectedParent {
                market_id: unit.market_id,
                expected: Box::new(current),
                held: Box::new(current),
            });
        }
        return Ok(Some(ProjectionCursor {
            range_end_block: unit.range_end_block,
            ..current
        }));
    };

    let position = last;
    if unit.range_end_block < position.anchor().number() {
        return Err(ProjectionError::RangeNotForward {
            market_id: unit.market_id,
            held: position.anchor().number(),
            arrived: unit.range_end_block,
        });
    }
    let range_end = index_column(unit.range_end_block, "range end")?;
    let block_number = index_column(position.anchor().number(), "block number")?;
    let block_hash = encode_hash(position.anchor().hash());
    let parent_hash = encode_hash(position.anchor().parent_hash());
    let tx_hash = encode_hash(position.tx_hash());
    let tx_index = index_column(position.tx_index(), "tx index")?;
    let log_index = index_column(position.log_index(), "log index")?;
    let chain_column = chain_id_column(chain)?;
    let cursor = ProjectionCursor {
        market_id: unit.market_id,
        position,
        journal_sequence: sequence,
        range_end_block: unit.range_end_block,
    };
    match held {
        None => {
            if let Some(Some(replaces)) = numeric_claim {
                require_range_forward(unit, replaces)?;
            }
            let inserted = conn.execute(
                "INSERT INTO aave_v3_projection_cursors \
                    (market_id, chain_id, block_number, block_hash, parent_hash, tx_hash, \
                     tx_index, log_index, journal_sequence, range_end_block) \
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10 \
                 WHERE NOT EXISTS (SELECT 1 FROM aave_v3_projection_cursors WHERE market_id = ?1)",
                params![
                    unit.market_id,
                    chain_column,
                    block_number,
                    block_hash,
                    parent_hash,
                    tx_hash,
                    tx_index,
                    log_index,
                    sequence,
                    range_end,
                ],
            )?;
            if inserted != 1 {
                return Err(ProjectionError::CursorAlreadyEstablished {
                    market_id: unit.market_id,
                });
            }
        }
        Some(current) => {
            require_range_forward(unit, current.range_end_block)?;
            let advanced = conn.execute(
                "UPDATE aave_v3_projection_cursors SET chain_id = ?2, block_number = ?3, \
                     block_hash = ?4, parent_hash = ?5, tx_hash = ?6, tx_index = ?7, \
                     log_index = ?8, journal_sequence = ?9, range_end_block = ?10 \
                 WHERE market_id = ?1 AND block_hash = ?11 AND tx_hash = ?12 \
                   AND log_index = ?13 AND journal_sequence = ?14",
                params![
                    unit.market_id,
                    chain_column,
                    block_number,
                    block_hash,
                    parent_hash,
                    tx_hash,
                    tx_index,
                    log_index,
                    sequence,
                    range_end,
                    encode_hash(current.position.anchor().hash()),
                    encode_hash(current.position.tx_hash()),
                    index_column(current.position.log_index(), "log index")?,
                    current.journal_sequence,
                ],
            )?;
            if advanced != 1 {
                return Err(ProjectionError::UnexpectedParent {
                    market_id: unit.market_id,
                    expected: Box::new(current),
                    held: Box::new(cursor),
                });
            }
        }
    }
    Ok(Some(cursor))
}

/// The unit's numeric range claim must advance past the committed one: a range
/// end that does not move is a replay, not a commit.
fn require_range_forward(unit: &ProjectionUnit<'_>, held: u64) -> Result<(), ProjectionError> {
    if unit.range_end_block <= held {
        return Err(ProjectionError::RangeNotForward {
            market_id: unit.market_id,
            held,
            arrived: unit.range_end_block,
        });
    }
    Ok(())
}
