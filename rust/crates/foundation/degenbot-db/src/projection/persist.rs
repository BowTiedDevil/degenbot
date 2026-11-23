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
    encode_hash, read_projection_cursor_on_conn, CheckpointPositionState, CursorAnchor,
    DeltaJournalEntry, GenerationActivation, GenerationRecord, LogCoordinate, ProjectionCheckpoint,
    ProjectionCursor, ProjectionError, ProjectionUnit,
};
use crate::rows::decode::decode_u256;
use crate::schema::ScaledTokenPosition;

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

/// One batched statement per [`IDENTITY_BATCH_ROWS`] rows.
fn insert_batched(
    conn: &Connection,
    verb: &str,
    into: &str,
    columns: usize,
    rows: &[Vec<rusqlite::types::Value>],
) -> Result<(), ProjectionError> {
    for batch in rows.chunks(IDENTITY_BATCH_ROWS) {
        let sql = format!(
            "{verb} {into} VALUES {}",
            placeholders(batch.len(), columns)
        );
        let values: Vec<rusqlite::types::Value> =
            batch.iter().flat_map(|row| row.iter().cloned()).collect();
        conn.execute(&sql, rusqlite::params_from_iter(values))?;
    }
    Ok(())
}

/// An identity batch: the same identity may be named by many units, so the
/// insert is idempotent and the writer verifies what it recorded.
fn insert_rows(
    conn: &Connection,
    into: &str,
    columns: usize,
    rows: &[Vec<rusqlite::types::Value>],
) -> Result<(), ProjectionError> {
    insert_batched(conn, "INSERT OR IGNORE", into, columns, rows)
}

/// An exact batch: a checkpoint's own rows are written once, so a duplicate is a
/// contradiction rather than something to ignore.
fn insert_rows_exact(
    conn: &Connection,
    into: &str,
    columns: usize,
    rows: &[Vec<rusqlite::types::Value>],
) -> Result<(), ProjectionError> {
    insert_batched(conn, "INSERT", into, columns, rows)
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
        require_generation(unit.market_id, generation, entry.position)?;
    }
    Ok(())
}

/// One generation relationship a unit or checkpoint names must be readable at the
/// position that names it: a non-empty entity, and — for an event-activated
/// generation — an activation at or before that position (ADR-105 D6).
fn require_generation(
    market_id: i64,
    generation: &GenerationRecord,
    position: LogCoordinate,
) -> Result<(), ProjectionError> {
    if generation.entity.is_empty() {
        return Err(ProjectionError::InvalidGenerationEntity { market_id });
    }
    let GenerationActivation::Event(at) = generation.activation else {
        return Ok(());
    };
    if !is_readable_at(at, position) {
        return Err(ProjectionError::GenerationNotForward {
            market_id,
            entity: generation.entity.clone(),
        });
    }
    Ok(())
}

/// Whether a generation activated at `at` is readable at `position`: same chain,
/// and at or before the position on the same branch (ADR-105 D6 — a replacement
/// creates a new generation at its exact event position rather than rewriting
/// what an earlier event meant).
fn is_readable_at(at: LogCoordinate, position: LogCoordinate) -> bool {
    at.anchor().chain_id() == position.anchor().chain_id()
        && at.anchor().number() <= position.anchor().number()
        && (at.anchor().number() < position.anchor().number()
            || (at.anchor().hash() == position.anchor().hash()
                && (at.tx_index(), at.log_index()) <= (position.tx_index(), position.log_index())))
}

/// Check that every generation a checkpoint names is readable at the checkpoint's
/// own position: a checkpoint cannot read under a generation activated later
/// than the cursor it captures.
fn validate_checkpoint_generations(
    market_id: i64,
    position: LogCoordinate,
    generations: &[GenerationRecord],
) -> Result<(), ProjectionError> {
    for generation in generations {
        require_generation(market_id, generation, position)?;
    }
    Ok(())
}

/// The bounded batch shape of one resume-state family: an owned row plus the
/// placeholder count it binds.
fn value_rows<T>(
    items: &[T],
    to_row: impl Fn(usize, &T) -> Vec<rusqlite::types::Value>,
) -> Vec<Vec<rusqlite::types::Value>> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| to_row(index, item))
        .collect()
}

/// Write the resolution state a checkpoint must carry beside its positions and
/// generations: the reserve identities, the contract endpoints and revisions,
/// the GHO row's link and discount columns, and the per-user discount state
/// (ADR-105 D8, D9).
///
/// These are the reads resolution makes at the unit's boundary, so a resume
/// classifies GHO initialization, discount paths, and replacement generations
/// from the checkpoint alone rather than from rows that may sit on an abandoned
/// branch.
fn write_checkpoint_resume_state(
    conn: &Connection,
    unit: &ProjectionUnit<'_>,
    checkpoint_id: i64,
    checkpoint: &ProjectionCheckpoint<'_>,
) -> Result<(), ProjectionError> {
    conn.execute(
        "INSERT INTO aave_v3_checkpoint_assets \
            (checkpoint_id, asset_id, underlying_address, a_token_address, v_token_address, \
             a_token_revision, v_token_revision, e_mode_category_id, price_source, \
             last_update_block, liquidity_index, liquidity_rate, borrow_index, borrow_rate) \
         SELECT ?1, a.id, u.address, at.address, vt.address, a.a_token_revision, \
                a.v_token_revision, a.e_mode_category_id, a.price_source, a.last_update_block, \
                a.liquidity_index, a.liquidity_rate, a.borrow_index, a.borrow_rate \
         FROM aave_v3_assets a \
         JOIN erc20_tokens u ON u.id = a.underlying_asset_id \
         JOIN erc20_tokens at ON at.id = a.a_token_id \
         JOIN erc20_tokens vt ON vt.id = a.v_token_id \
         WHERE a.market_id = ?2 ORDER BY a.id",
        params![checkpoint_id, unit.market_id],
    )?;
    conn.execute(
        "INSERT INTO aave_v3_checkpoint_asset_configs \
            (checkpoint_id, asset_id, ltv, liquidation_threshold, liquidation_bonus, \
             e_mode_category_id, borrowing_enabled, stable_borrowing_enabled, \
             flash_loan_enabled, isolation_mode, borrowable_in_isolation, debt_ceiling) \
         SELECT ?1, c.asset_id, c.ltv, c.liquidation_threshold, c.liquidation_bonus, \
                c.e_mode_category_id, c.borrowing_enabled, c.stable_borrowing_enabled, \
                c.flash_loan_enabled, c.isolation_mode, c.borrowable_in_isolation, c.debt_ceiling \
         FROM aave_v3_asset_configs c JOIN aave_v3_assets a ON a.id = c.asset_id \
         WHERE a.market_id = ?2 ORDER BY c.asset_id",
        params![checkpoint_id, unit.market_id],
    )?;

    let rows = value_rows(checkpoint.contracts, |_, contract| {
        vec![
            rusqlite::types::Value::Integer(checkpoint_id),
            rusqlite::types::Value::Text(contract.name.clone()),
            rusqlite::types::Value::Text(contract.address.clone()),
            contract
                .revision
                .map_or(rusqlite::types::Value::Null, |revision| {
                    rusqlite::types::Value::Integer(i64::from(revision))
                }),
        ]
    });
    insert_rows_exact(
        conn,
        "INTO aave_v3_checkpoint_contracts (checkpoint_id, name, address, revision)",
        4,
        &rows,
    )?;

    if let Some(gho) = checkpoint.gho {
        conn.execute(
            "INSERT INTO aave_v3_checkpoint_gho (checkpoint_id, gho_id, token_id, \
                v_token_id, gho_token_address, v_token_address, discount_rate_strategy, \
                discount_token) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                checkpoint_id,
                gho.gho_id,
                gho.token_id,
                gho.v_token_id,
                gho.gho_token_address,
                gho.v_token_address,
                gho.discount_rate_strategy,
                gho.discount_token,
            ],
        )?;
    }

    let users = market_users_on_conn(conn, checkpoint_id, unit.market_id)?;
    insert_rows_exact(
        conn,
        "INTO aave_v3_checkpoint_users (checkpoint_id, user_id, address, gho_discount, \
            stk_aave_balance, e_mode, isolation_mode_collateral_asset_id, isolation_mode_debt)",
        8,
        &users,
    )?;

    conn.execute(
        "INSERT INTO aave_v3_checkpoint_user_collaterals \
            (checkpoint_id, user_id, asset_id, enabled) \
         SELECT ?1, c.user_id, c.asset_id, c.enabled \
         FROM aave_v3_user_collateral_configs c \
         JOIN aave_v3_users u ON u.id = c.user_id \
         WHERE u.market_id = ?2 ORDER BY c.user_id, c.asset_id",
        params![checkpoint_id, unit.market_id],
    )?;
    conn.execute(
        "INSERT INTO aave_v3_checkpoint_emode_categories \
            (checkpoint_id, category_id, label, ltv, liquidation_threshold, \
             liquidation_bonus, price_source) \
         SELECT ?1, e.category_id, e.label, e.ltv, e.liquidation_threshold, \
                e.liquidation_bonus, e.price_source \
         FROM aave_v3_emode_categories e WHERE e.market_id = ?2 ORDER BY e.category_id",
        params![checkpoint_id, unit.market_id],
    )?;
    Ok(())
}

/// The market's complete user set at the cursor, with the two discount columns
/// resolution reads at transaction start. Storage is the only enumeration of the
/// set, so the unit writer reads it here rather than accepting a partial answer
/// from a candidate-scoped cache.
fn market_users_on_conn(
    conn: &Connection,
    checkpoint_id: i64,
    market_id: i64,
) -> Result<Vec<Vec<rusqlite::types::Value>>, ProjectionError> {
    let mut statement = conn.prepare(
        "SELECT id, address, gho_discount, stk_aave_balance, e_mode, \
                isolation_mode_collateral_asset_id, isolation_mode_debt \
         FROM aave_v3_users WHERE market_id = ?1 ORDER BY id",
    )?;
    let mut rows = statement.query(params![market_id])?;
    let mut values = Vec::new();
    while let Some(row) = rows.next()? {
        let balance: Option<String> = row.get(3)?;
        values.push(vec![
            rusqlite::types::Value::Integer(checkpoint_id),
            rusqlite::types::Value::Integer(row.get(0)?),
            rusqlite::types::Value::Text(row.get(1)?),
            rusqlite::types::Value::Integer(row.get(2)?),
            balance.map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Text),
            rusqlite::types::Value::Integer(row.get(4)?),
            row.get::<_, Option<i64>>(5)?.map_or(
                rusqlite::types::Value::Null,
                rusqlite::types::Value::Integer,
            ),
            rusqlite::types::Value::Text(row.get(6)?),
        ]);
    }
    Ok(values)
}

/// The market's settled position state at the unit's cursor: every row the two
/// position relations hold for the market, overlaid by the reducing view's own
/// state.
///
/// The storage rows are the truth the apply just wrote. The overlay exists for
/// the one state storage no longer holds: the end-of-chunk cleanup deletes a
/// position whose balance settled to zero, while the reducing view still reads
/// `(0, index)` for it — and a retained delta may name that position again. A
/// NON-zero overlay state with no row is therefore not this market's storage,
/// and is refused rather than written into the checkpoint.
fn checkpoint_positions_on_conn(
    conn: &Connection,
    market_id: i64,
    overlay: &[CheckpointPositionState],
) -> Result<Vec<CheckpointPositionState>, ProjectionError> {
    let mut statement = conn.prepare(
        "SELECT 'collateral', p.id, p.balance, p.last_index \
           FROM aave_v3_collateral_positions p \
           JOIN aave_v3_users u ON u.id = p.user_id \
          WHERE u.market_id = ?1 \
         UNION ALL \
         SELECT 'debt', p.id, p.balance, p.last_index \
           FROM aave_v3_debt_positions p \
           JOIN aave_v3_users u ON u.id = p.user_id \
          WHERE u.market_id = ?1",
    )?;
    let mut rows = statement.query(params![market_id])?;
    let mut positions: std::collections::BTreeMap<
        (ScaledTokenPosition, i64),
        CheckpointPositionState,
    > = std::collections::BTreeMap::new();
    while let Some(row) = rows.next()? {
        let code: String = row.get(0)?;
        let position = ScaledTokenPosition::from_code(&code)
            .ok_or_else(|| ProjectionError::Decode(format!("position relation {code:?}")))?;
        let position_id: i64 = row.get(1)?;
        let balance: String = row.get(2)?;
        let index: Option<String> = row.get(3)?;
        positions.insert(
            (position, position_id),
            CheckpointPositionState {
                position,
                position_id,
                balance: decode_u256(&balance).map_err(|error| {
                    ProjectionError::Decode(format!("position {position_id} balance: {error}"))
                })?,
                index: index
                    .map(|index| {
                        decode_u256(&index).map_err(|error| {
                            ProjectionError::Decode(format!(
                                "position {position_id} index: {error}"
                            ))
                        })
                    })
                    .transpose()?,
            },
        );
    }
    drop(rows);
    drop(statement);

    for state in overlay {
        let key = (state.position, state.position_id);
        if positions.contains_key(&key) {
            continue;
        }
        if !state.balance.is_zero() {
            return Err(ProjectionError::CheckpointPositionNotInMarket {
                market_id,
                position: state.position,
                position_id: state.position_id,
            });
        }
        positions.insert(key, *state);
    }
    Ok(positions.into_values().collect())
}

/// Commit one checkpoint at the cursor the unit just advanced to (ADR-105 D8,
/// D9).
///
/// The checkpoint's position, journal sequence, and range claim are the unit's
/// own — never arguments — so a checkpoint cannot name a cursor the unit did not
/// commit. It is written LAST, inside the caller's transaction: a refusal here
/// (or anywhere after) leaves the whole unit uncommitted.
fn write_checkpoint(
    conn: &Connection,
    unit: &ProjectionUnit<'_>,
    checkpoint: &ProjectionCheckpoint<'_>,
    cursor: &ProjectionCursor,
) -> Result<(), ProjectionError> {
    let position = cursor.position;
    validate_checkpoint_generations(unit.market_id, position, checkpoint.generations)?;
    let generation_ids = write_generations(conn, unit.market_id, checkpoint.generations)?;
    let positions = checkpoint_positions_on_conn(conn, unit.market_id, checkpoint.positions)?;

    conn.execute(
        "INSERT INTO aave_v3_projection_checkpoints \
            (market_id, journal_sequence, chain_id, block_number, block_hash, parent_hash, \
             tx_hash, tx_index, log_index, range_end_block, encoding_version) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            unit.market_id,
            cursor.journal_sequence,
            chain_id_column(position.anchor().chain_id())?,
            index_column(position.anchor().number(), "block number")?,
            encode_hash(position.anchor().hash()),
            encode_hash(position.anchor().parent_hash()),
            encode_hash(position.tx_hash()),
            index_column(position.tx_index(), "tx index")?,
            index_column(position.log_index(), "log index")?,
            index_column(cursor.range_end_block, "range end")?,
            checkpoint.encoding_version,
        ],
    )?;
    let checkpoint_id = conn.last_insert_rowid();

    let rows: Vec<Vec<rusqlite::types::Value>> = checkpoint
        .generations
        .iter()
        .map(|generation| {
            let generation_id = generation_ids
                .get(&generation_key(generation))
                .copied()
                .ok_or_else(|| ProjectionError::MissingGeneration {
                    market_id: unit.market_id,
                    entity: generation.entity.clone(),
                })?;
            Ok(vec![
                rusqlite::types::Value::Integer(checkpoint_id),
                rusqlite::types::Value::Integer(unit.market_id),
                rusqlite::types::Value::Integer(generation_id),
            ])
        })
        .collect::<Result<Vec<_>, ProjectionError>>()?;
    insert_rows_exact(
        conn,
        "INTO aave_v3_checkpoint_generations (checkpoint_id, market_id, generation_id)",
        3,
        &rows,
    )?;

    let rows: Vec<Vec<rusqlite::types::Value>> = positions
        .iter()
        .map(|state| {
            vec![
                rusqlite::types::Value::Integer(checkpoint_id),
                rusqlite::types::Value::Text(state.position.code().to_string()),
                rusqlite::types::Value::Integer(state.position_id),
                rusqlite::types::Value::Text(state.balance.to_string()),
                state.index.map_or(rusqlite::types::Value::Null, |index| {
                    rusqlite::types::Value::Text(index.to_string())
                }),
            ]
        })
        .collect();
    insert_rows_exact(
        conn,
        "INTO aave_v3_checkpoint_positions \
            (checkpoint_id, position, position_id, balance, last_index)",
        5,
        &rows,
    )?;
    write_checkpoint_resume_state(conn, unit, checkpoint_id, checkpoint)?;
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
    market_id: i64,
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
            rusqlite::types::Value::Integer(market_id),
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
    let mut cursor = statement.query(params![market_id])?;
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
                market_id,
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

    if let Some(checkpoint) = unit.checkpoint {
        if unit.entries.is_empty() {
            return Err(ProjectionError::CheckpointWithoutCursor {
                market_id: unit.market_id,
            });
        }
        if checkpoint.encoding_version.is_empty() {
            return Err(ProjectionError::CheckpointWithoutVersion {
                market_id: unit.market_id,
            });
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
    let generation_ids = write_generations(conn, unit.market_id, &identities.generations)?;
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
    if let Some(checkpoint) = unit.checkpoint {
        write_checkpoint(conn, unit, &checkpoint, &cursor)?;
    }
    prune_retention(conn, unit.market_id, position.anchor().number())?;
    Ok(Some(cursor))
}

/// Prune the evidence that has fallen out of the session reorg window, so a
/// market's journal and checkpoints are bounded by the window rather than by how
/// long the market has run (ADR-105 D9).
///
/// The newest checkpoint at or below the window's floor is the deepest restore
/// point a recovery can use, so it is retained: the oldest retained journal row
/// keeps its own checkpoint, and every row (and checkpoint) before it is
/// evidence no recovery can reach.
fn prune_retention(conn: &Connection, market_id: i64, head: u64) -> Result<(), ProjectionError> {
    let Some(floor) = head.checked_sub(crate::projection::REORG_WINDOW_BLOCKS) else {
        return Ok(());
    };
    let outside: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM aave_v3_delta_journal WHERE market_id = ?1 \
         AND block_number <= ?2)",
        params![market_id, index_column(floor, "window floor")?],
        |row| row.get(0),
    )?;
    if !outside {
        return Ok(());
    }
    let oldest_inside: i64 = conn.query_row(
        "SELECT MIN(sequence) FROM aave_v3_delta_journal WHERE market_id = ?1 \
         AND block_number > ?2",
        params![market_id, index_column(floor, "window floor")?],
        |row| row.get(0),
    )?;
    for table in super::recovery::CHECKPOINT_CHILD_TABLES {
        conn.execute(
            &format!(
                "DELETE FROM {table} WHERE checkpoint_id IN (SELECT id FROM \
                 aave_v3_projection_checkpoints WHERE market_id = ?1 AND journal_sequence < ?2)"
            ),
            params![market_id, oldest_inside],
        )?;
    }
    conn.execute(
        "DELETE FROM aave_v3_projection_checkpoints WHERE market_id = ?1 \
         AND journal_sequence < ?2",
        params![market_id, oldest_inside],
    )?;
    conn.execute(
        "DELETE FROM aave_v3_delta_journal WHERE market_id = ?1 AND sequence < ?2",
        params![market_id, oldest_inside],
    )?;
    Ok(())
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
