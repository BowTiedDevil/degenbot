//! Projection recovery: the retained-window reads a recovery plans from and the
//! market-scoped restore it commits (ADR-105 D9).
//!
//! Recovery never inverts a delta or a protocol operation. It restores the state
//! a retained checkpoint states and truncates the evidence the abandoned branch
//! produced above the common ancestor, so the market's journal continues from a
//! position the new branch shares and the new branch's deltas apply forward from
//! there.
//!
//! Every write here is scoped to one market, and the cursor rewind is fenced by
//! the exact cursor the caller read: a competing advance, a reorg, or a replay is
//! the same [`ProjectionError::UnexpectedParent`] refusal an ordinary unit gets.
//! The caller supplies the transaction, so a refusal anywhere leaves the database
//! byte-identical and no partially restored market is observable.

use rusqlite::{params, Connection};

use super::{decode_journal_row, DeltaJournalRow, LogCoordinate, ProjectionError};
#[cfg(any(test, feature = "writer"))]
use super::{
    read_projection_cursor_on_conn, CheckpointAsset, CheckpointContract, CheckpointGho,
    CheckpointPositionState, CheckpointUser, ProjectionCursor,
};
#[cfg(any(test, feature = "writer"))]
use crate::schema::ScaledTokenPosition;

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests;

/// One retained checkpoint's boundary: the journal sequence it captures and the
/// exact position that sequence terminates at.
///
/// A recovery selects its restore point from these alone, so selection never
/// reads a checkpoint payload or a market row the abandoned branch may have
/// written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointBoundary {
    /// The journal entry the checkpointed cursor terminates at.
    pub journal_sequence: i64,
    /// The exact position the checkpoint restores to.
    pub position: LogCoordinate,
}

/// Every retained checkpoint of `market_id`, in journal-sequence order.
///
/// # Errors
///
/// [`ProjectionError::Sqlite`] on a query failure, and
/// [`ProjectionError::Decode`] when a stored coordinate is not an exact identity.
pub fn read_retained_checkpoints_on_conn(
    conn: &Connection,
    market_id: i64,
) -> Result<Vec<CheckpointBoundary>, ProjectionError> {
    let mut statement = conn.prepare(
        "SELECT journal_sequence, chain_id, block_number, block_hash, parent_hash, tx_hash, \
                tx_index, log_index \
         FROM aave_v3_projection_checkpoints WHERE market_id = ?1 ORDER BY journal_sequence",
    )?;
    let mut rows = statement.query(params![market_id])?;
    let mut boundaries = Vec::new();
    while let Some(row) = rows.next()? {
        boundaries.push(CheckpointBoundary {
            journal_sequence: row.get(0)?,
            position: decode_coordinate(row, 1)?,
        });
    }
    Ok(boundaries)
}

/// The journaled deltas of `market_id` after `after_sequence`, in sequence order:
/// the retained evidence a recovery replays forward.
///
/// # Errors
///
/// [`ProjectionError::Sqlite`] on a query failure, and
/// [`ProjectionError::Decode`] when a stored coordinate or generation is not a
/// value this schema's writer could have written.
pub fn read_retained_journal_on_conn(
    conn: &Connection,
    market_id: i64,
    after_sequence: i64,
) -> Result<Vec<DeltaJournalRow>, ProjectionError> {
    let mut statement = conn.prepare(
        "SELECT j.sequence, j.position_ordinal, j.chain_id, j.block_number, j.block_hash, \
                j.parent_hash, j.tx_hash, j.tx_index, j.log_index, j.encoding_version, \
                j.delta_encoding, g.entity, g.activation_kind, g.revision, g.chain_id, \
                g.block_number, g.block_hash, g.parent_hash, g.tx_hash, g.tx_index, g.log_index \
         FROM aave_v3_delta_journal j \
         LEFT JOIN aave_v3_generations g \
           ON g.id = j.generation_id AND g.market_id = j.market_id \
         WHERE j.market_id = ?1 AND j.sequence > ?2 ORDER BY j.sequence",
    )?;
    let mut rows = statement.query(params![market_id, after_sequence])?;
    let mut journal = Vec::new();
    while let Some(row) = rows.next()? {
        journal.push(decode_journal_row(row)?);
    }
    Ok(journal)
}

/// The exact coordinate a row's block/transaction/log columns name, when the
/// block columns start at `first`.
fn decode_coordinate(
    row: &rusqlite::Row<'_>,
    first: usize,
) -> Result<LogCoordinate, ProjectionError> {
    let anchor = super::decode_anchor(
        row.get::<_, i64>(first)?,
        row.get::<_, i64>(first + 1)?,
        &row.get::<_, String>(first + 2)?,
        &row.get::<_, String>(first + 3)?,
    )?;
    Ok(LogCoordinate::new(
        anchor,
        super::decode_hash(&row.get::<_, String>(first + 4)?)?,
        u64::try_from(row.get::<_, i64>(first + 5)?)
            .map_err(|_| ProjectionError::Decode("tx index".to_string()))?,
        u64::try_from(row.get::<_, i64>(first + 6)?)
            .map_err(|_| ProjectionError::Decode("log index".to_string()))?,
    ))
}

/// The state a recovery restores beside the cursor it rewinds to: exactly the
/// families a retained checkpoint states.
///
/// The position family is complete (the checkpoint's writer enumerates every
/// position the market holds), the contract and GHO families carry those
/// relations' own facts, and the user and reserve families carry the facts the
/// checkpoint states — a user's discount columns and a reserve's revisions.
#[cfg(any(test, feature = "writer"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct RecoveryRestore<'a> {
    /// The settled per-position state at the restored cursor.
    pub positions: &'a [CheckpointPositionState],
    /// The reserves' identities, revisions, and settled configuration.
    pub assets: &'a [CheckpointAsset],
    /// The reserves' settled collateral configurations.
    pub asset_configs: &'a [super::CheckpointAssetConfig],
    /// The contract endpoints and revisions the projection read under.
    pub contracts: &'a [CheckpointContract],
    /// The chain's GHO row, when the chain has one.
    pub gho: Option<&'a CheckpointGho>,
    /// The per-user discount and isolation state resolution reads at transaction
    /// start.
    pub users: &'a [CheckpointUser],
    /// The users' settled collateral flags at the restored cursor.
    pub user_collaterals: &'a [super::CheckpointUserCollateral],
    /// The e-mode category definitions in force at the restored cursor.
    pub emode_categories: &'a [super::CheckpointEModeCategory],
}

/// One market's recovery: the cursor the caller read, the common ancestor the
/// market's journal still retains, and the state to restore there.
#[cfg(any(test, feature = "writer"))]
#[derive(Debug, Clone, Copy)]
pub struct ProjectionRecovery<'a> {
    /// The market being recovered.
    pub market_id: i64,
    /// The exact cursor the caller read before planning the recovery.
    pub expected: ProjectionCursor,
    /// The deepest retained position the new head's ancestry still holds.
    pub ancestor: LogCoordinate,
    /// The journal entry `ancestor` terminates at.
    pub ancestor_sequence: i64,
    /// The numeric range end the projection covered at the ancestor.
    pub range_end_block: u64,
    /// The checkpoint state restored at the ancestor.
    pub restore: RecoveryRestore<'a>,
}

/// The checkpoint-owned tables a recovery prunes with the checkpoint itself.
#[cfg(any(test, feature = "writer"))]
pub(super) const CHECKPOINT_CHILD_TABLES: &[&str] = &[
    "aave_v3_checkpoint_generations",
    "aave_v3_checkpoint_positions",
    "aave_v3_checkpoint_assets",
    "aave_v3_checkpoint_asset_configs",
    "aave_v3_checkpoint_contracts",
    "aave_v3_checkpoint_gho",
    "aave_v3_checkpoint_users",
    "aave_v3_checkpoint_user_collaterals",
    "aave_v3_checkpoint_emode_categories",
];

/// Restore `market_id` to its common ancestor: prune the checkpoints and journal
/// entries the abandoned branch produced, rewind both the exact cursor and the
/// numeric range claim, and write the restored state — all inside the caller's
/// transaction (ADR-105 D9). A numeric cursor left at the abandoned head would
/// make the next run resume above the ancestor and skip the replaced range.
///
/// Nothing is written until every check has passed, so a refusal leaves the
/// market byte-identical and no partial restoration exists. The returned cursor is
/// the market's after the rewind.
///
/// # Errors
///
/// [`ProjectionError::UnexpectedParent`] when the cursor is not the one the caller
/// read, [`ProjectionError::RecoveryAncestorNotRetained`] when the retained journal
/// holds no entry at the claimed ancestor sequence,
/// [`ProjectionError::RecoveryAncestorNotHeld`] when the ancestor is not at or
/// before the committed cursor, a `Recovery*NotIn*` refusal when the restored state
/// names a row the market does not hold, and [`ProjectionError::Sqlite`] on a write
/// failure.
#[cfg(any(test, feature = "writer"))]
#[expect(clippy::too_many_lines)]
pub fn restore_projection_on_conn(
    conn: &Connection,
    recovery: &ProjectionRecovery<'_>,
) -> Result<ProjectionCursor, ProjectionError> {
    let market_id = recovery.market_id;
    let held = read_projection_cursor_on_conn(conn, market_id)?
        .ok_or(ProjectionError::CursorMissing { market_id })?;
    if held != recovery.expected {
        return Err(ProjectionError::UnexpectedParent {
            market_id,
            expected: Box::new(recovery.expected),
            held: Box::new(held),
        });
    }
    if recovery.ancestor.is_forward_of(&held.position)
        || recovery.ancestor.contradicts_branch_of(&held.position)
        || recovery.ancestor_sequence > held.journal_sequence
    {
        return Err(ProjectionError::RecoveryAncestorNotHeld { market_id });
    }
    let retained: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM aave_v3_delta_journal WHERE market_id = ?1 \
         AND sequence = ?2 AND chain_id = ?3 AND block_number = ?4 AND block_hash = ?5 \
         AND parent_hash = ?6 AND tx_hash = ?7 AND tx_index = ?8 AND log_index = ?9)",
        params![
            market_id,
            recovery.ancestor_sequence,
            i64::try_from(recovery.ancestor.anchor().chain_id()).unwrap_or(0),
            i64::try_from(recovery.ancestor.anchor().number()).unwrap_or(i64::MAX),
            super::encode_hash(recovery.ancestor.anchor().hash()),
            super::encode_hash(recovery.ancestor.anchor().parent_hash()),
            super::encode_hash(recovery.ancestor.tx_hash()),
            i64::try_from(recovery.ancestor.tx_index()).unwrap_or(i64::MAX),
            i64::try_from(recovery.ancestor.log_index()).unwrap_or(i64::MAX),
        ],
        |row| row.get(0),
    )?;
    if !retained {
        return Err(ProjectionError::RecoveryAncestorNotRetained {
            market_id,
            sequence: recovery.ancestor_sequence,
        });
    }

    for table in CHECKPOINT_CHILD_TABLES {
        conn.execute(
            &format!(
                "DELETE FROM {table} WHERE checkpoint_id IN (SELECT id FROM \
                 aave_v3_projection_checkpoints WHERE market_id = ?1 AND journal_sequence > ?2)"
            ),
            params![market_id, recovery.ancestor_sequence],
        )?;
    }
    conn.execute(
        "DELETE FROM aave_v3_projection_checkpoints WHERE market_id = ?1 \
         AND journal_sequence > ?2",
        params![market_id, recovery.ancestor_sequence],
    )?;

    let rewound = conn.execute(
        "UPDATE aave_v3_projection_cursors SET chain_id = ?2, block_number = ?3, \
             block_hash = ?4, parent_hash = ?5, tx_hash = ?6, tx_index = ?7, log_index = ?8, \
             journal_sequence = ?9, range_end_block = ?10 \
         WHERE market_id = ?11 AND block_hash = ?12 AND tx_hash = ?13 AND log_index = ?14 \
           AND journal_sequence = ?15",
        params![
            market_id,
            i64::try_from(recovery.ancestor.anchor().chain_id()).unwrap_or(0),
            i64::try_from(recovery.ancestor.anchor().number()).unwrap_or(i64::MAX),
            super::encode_hash(recovery.ancestor.anchor().hash()),
            super::encode_hash(recovery.ancestor.anchor().parent_hash()),
            super::encode_hash(recovery.ancestor.tx_hash()),
            i64::try_from(recovery.ancestor.tx_index()).unwrap_or(i64::MAX),
            i64::try_from(recovery.ancestor.log_index()).unwrap_or(i64::MAX),
            recovery.ancestor_sequence,
            i64::try_from(recovery.range_end_block).unwrap_or(i64::MAX),
            market_id,
            super::encode_hash(held.position.anchor().hash()),
            super::encode_hash(held.position.tx_hash()),
            i64::try_from(held.position.log_index()).unwrap_or(i64::MAX),
            held.journal_sequence,
        ],
    )?;
    if rewound != 1 {
        return Err(ProjectionError::UnexpectedParent {
            market_id,
            expected: Box::new(recovery.expected),
            held: Box::new(held),
        });
    }

    let rewound_numeric = conn.execute(
        "UPDATE aave_v3_markets SET last_update_block = ?2 WHERE id = ?1",
        params![
            market_id,
            i64::try_from(recovery.range_end_block).unwrap_or(i64::MAX),
        ],
    )?;
    if rewound_numeric != 1 {
        return Err(ProjectionError::MissingMarket(market_id));
    }

    conn.execute(
        "DELETE FROM aave_v3_delta_journal WHERE market_id = ?1 AND sequence > ?2",
        params![market_id, recovery.ancestor_sequence],
    )?;

    restore_positions(conn, market_id, recovery.restore.positions)?;
    restore_users(conn, market_id, recovery.restore.users)?;
    restore_contracts(conn, market_id, recovery.restore.contracts)?;
    restore_assets(conn, market_id, recovery.restore.assets)?;
    restore_asset_configs(conn, market_id, recovery.restore.asset_configs)?;
    restore_user_collaterals(conn, market_id, recovery.restore.user_collaterals)?;
    restore_emode_categories(conn, market_id, recovery.restore.emode_categories)?;
    if let Some(gho) = recovery.restore.gho {
        let written = conn.execute(
            "UPDATE aave_gho_tokens SET v_token_id = ?2, v_gho_discount_rate_strategy = ?3, \
                 v_gho_discount_token = ?4 WHERE id = ?1",
            params![
                gho.gho_id,
                gho.v_token_id,
                gho.discount_rate_strategy,
                gho.discount_token,
            ],
        )?;
        if written != 1 {
            return Err(ProjectionError::RecoveryGhoNotInChain { gho_id: gho.gho_id });
        }
    }

    Ok(ProjectionCursor {
        market_id,
        position: recovery.ancestor,
        journal_sequence: recovery.ancestor_sequence,
        range_end_block: recovery.range_end_block,
    })
}

/// Write the restored position family: every non-zero settled balance is set on
/// its row, and every position row the restored state does not hold (or holds at
/// zero) is deleted, because a row that does not exist at the ancestor cannot be
/// read by the new branch's resolution.
#[cfg(any(test, feature = "writer"))]
fn restore_positions(
    conn: &Connection,
    market_id: i64,
    positions: &[CheckpointPositionState],
) -> Result<(), ProjectionError> {
    for state in positions {
        if state.balance.is_zero() {
            continue;
        }
        let written = conn.execute(
            &format!(
                "UPDATE {} SET balance = ?2, last_index = ?3 WHERE id = ?1",
                state.position.table()
            ),
            params![
                state.position_id,
                state.balance.to_string(),
                state.index.map(|index| index.to_string()),
            ],
        )?;
        if written != 1 {
            return Err(ProjectionError::CheckpointPositionNotInMarket {
                market_id,
                position: state.position,
                position_id: state.position_id,
            });
        }
    }
    for relation in ScaledTokenPosition::ALL {
        let kept: Vec<i64> = positions
            .iter()
            .filter(|state| state.position == *relation && !state.balance.is_zero())
            .map(|state| state.position_id)
            .collect();
        let predicate = if kept.is_empty() {
            String::new()
        } else {
            format!(
                " AND id NOT IN ({})",
                kept.iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        conn.execute(
            &format!(
                "DELETE FROM {} WHERE user_id IN (SELECT id FROM aave_v3_users \
                 WHERE market_id = ?1){predicate}",
                relation.table()
            ),
            params![market_id],
        )?;
    }
    Ok(())
}

/// Write the restored per-user discount state the resolution pre-pass reads.
#[cfg(any(test, feature = "writer"))]
fn restore_users(
    conn: &Connection,
    market_id: i64,
    users: &[CheckpointUser],
) -> Result<(), ProjectionError> {
    for user in users {
        let written = conn.execute(
            "UPDATE aave_v3_users SET gho_discount = ?3, stk_aave_balance = ?4, e_mode = ?5, \
                 isolation_mode_collateral_asset_id = ?6, isolation_mode_debt = ?7 \
             WHERE id = ?1 AND market_id = ?2",
            params![
                user.user_id,
                market_id,
                user.gho_discount,
                user.stk_aave_balance.map(|balance| balance.to_string()),
                user.e_mode,
                user.isolation_mode_collateral_asset_id,
                user.isolation_mode_debt.to_string(),
            ],
        )?;
        if written != 1 {
            return Err(ProjectionError::RecoveryUserNotInMarket {
                market_id,
                user_id: user.user_id,
            });
        }
    }
    Ok(())
}

/// Write the restored contract endpoints and revisions resolution targets.
#[cfg(any(test, feature = "writer"))]
fn restore_contracts(
    conn: &Connection,
    market_id: i64,
    contracts: &[CheckpointContract],
) -> Result<(), ProjectionError> {
    for contract in contracts {
        let written = conn.execute(
            "UPDATE aave_v3_contracts SET address = ?3, revision = ?4 \
             WHERE market_id = ?1 AND name = ?2",
            params![
                market_id,
                contract.name,
                contract.address,
                contract.revision,
            ],
        )?;
        if written != 1 {
            return Err(ProjectionError::RecoveryContractNotInMarket {
                market_id,
                role: contract.name.clone(),
            });
        }
    }
    Ok(())
}

/// Write the restored collateral-configuration family: every reserve's settled
/// bitmap-decoded fields are set, and a reserve whose configuration the restored
/// cursor does not hold has its row removed, so an abandoned branch's
/// configuration cannot survive the rewind.
#[cfg(any(test, feature = "writer"))]
fn restore_asset_configs(
    conn: &Connection,
    market_id: i64,
    configs: &[super::CheckpointAssetConfig],
) -> Result<(), ProjectionError> {
    for config in configs {
        let written = conn.execute(
            "UPDATE aave_v3_asset_configs SET ltv = ?2, liquidation_threshold = ?3, \
                 liquidation_bonus = ?4, e_mode_category_id = ?5, borrowing_enabled = ?6, \
                 stable_borrowing_enabled = ?7, flash_loan_enabled = ?8, isolation_mode = ?9, \
                 borrowable_in_isolation = ?10, debt_ceiling = ?11 \
             WHERE asset_id = ?1 \
               AND asset_id IN (SELECT id FROM aave_v3_assets WHERE market_id = ?12)",
            params![
                config.asset_id,
                config.ltv,
                config.liquidation_threshold,
                config.liquidation_bonus,
                config.e_mode_category_id,
                config.borrowing_enabled,
                config.stable_borrowing_enabled,
                config.flash_loan_enabled,
                config.isolation_mode,
                config.borrowable_in_isolation,
                config.debt_ceiling.map(|ceiling| ceiling.to_string()),
                market_id,
            ],
        )?;
        if written != 1 {
            return Err(ProjectionError::RecoveryAssetNotInMarket {
                market_id,
                asset_id: config.asset_id,
            });
        }
    }
    let kept: Vec<i64> = configs.iter().map(|config| config.asset_id).collect();
    let predicate = if kept.is_empty() {
        String::new()
    } else {
        format!(
            " AND c.asset_id NOT IN ({})",
            kept.iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    conn.execute(
        &format!(
            "DELETE FROM aave_v3_asset_configs WHERE id IN (SELECT c.id FROM aave_v3_asset_configs c \
             JOIN aave_v3_assets a ON a.id = c.asset_id WHERE a.market_id = ?1{predicate})"
        ),
        params![market_id],
    )?;
    Ok(())
}

/// Write the restored collateral flags: the users' `ReserveUsedAsCollateral`
/// state at the ancestor, with any flag the ancestor's state does not hold
/// removed so an abandoned enable cannot leak forward.
#[cfg(any(test, feature = "writer"))]
fn restore_user_collaterals(
    conn: &Connection,
    market_id: i64,
    collaterals: &[super::CheckpointUserCollateral],
) -> Result<(), ProjectionError> {
    for collateral in collaterals {
        let written = conn.execute(
            "UPDATE aave_v3_user_collateral_configs SET enabled = ?3 \
             WHERE user_id = ?1 AND asset_id = ?2 \
               AND user_id IN (SELECT id FROM aave_v3_users WHERE market_id = ?4) \
               AND asset_id IN (SELECT id FROM aave_v3_assets WHERE market_id = ?4)",
            params![
                collateral.user_id,
                collateral.asset_id,
                collateral.enabled,
                market_id,
            ],
        )?;
        if written == 0 {
            conn.execute(
                "INSERT INTO aave_v3_user_collateral_configs (user_id, asset_id, enabled) \
                 SELECT ?1, ?2, ?3 \
                 WHERE EXISTS (SELECT 1 FROM aave_v3_users WHERE id = ?1 AND market_id = ?4) \
                   AND EXISTS (SELECT 1 FROM aave_v3_assets WHERE id = ?2 AND market_id = ?4)",
                params![
                    collateral.user_id,
                    collateral.asset_id,
                    collateral.enabled,
                    market_id,
                ],
            )?;
        }
    }
    let kept: Vec<String> = collaterals
        .iter()
        .map(|collateral| format!("({}, {})", collateral.user_id, collateral.asset_id))
        .collect();
    let predicate = if kept.is_empty() {
        String::new()
    } else {
        format!(" AND (c.user_id, c.asset_id) NOT IN ({})", kept.join(", "))
    };
    conn.execute(
        &format!(
            "DELETE FROM aave_v3_user_collateral_configs WHERE id IN ( \
                 SELECT c.id FROM aave_v3_user_collateral_configs c \
                 JOIN aave_v3_users u ON u.id = c.user_id \
                 WHERE u.market_id = ?1{predicate})"
        ),
        params![market_id],
    )?;
    Ok(())
}

/// Write the restored e-mode categories: the definitions the ancestor held,
/// with any category the ancestor's state does not hold removed.
#[cfg(any(test, feature = "writer"))]
fn restore_emode_categories(
    conn: &Connection,
    market_id: i64,
    categories: &[super::CheckpointEModeCategory],
) -> Result<(), ProjectionError> {
    for category in categories {
        let updated = conn.execute(
            "UPDATE aave_v3_emode_categories SET label = ?3, ltv = ?4, \
                 liquidation_threshold = ?5, liquidation_bonus = ?6, price_source = ?7 \
             WHERE market_id = ?1 AND category_id = ?2",
            params![
                market_id,
                category.category_id,
                category.label,
                category.ltv,
                category.liquidation_threshold,
                category.liquidation_bonus,
                category.price_source,
            ],
        )?;
        if updated == 0 {
            conn.execute(
                "INSERT INTO aave_v3_emode_categories \
                    (market_id, category_id, label, ltv, liquidation_threshold, \
                     liquidation_bonus, price_source) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    market_id,
                    category.category_id,
                    category.label,
                    category.ltv,
                    category.liquidation_threshold,
                    category.liquidation_bonus,
                    category.price_source,
                ],
            )?;
        }
    }
    let kept: Vec<String> = categories
        .iter()
        .map(|category| category.category_id.to_string())
        .collect();
    let predicate = if kept.is_empty() {
        String::new()
    } else {
        format!(" AND category_id NOT IN ({})", kept.join(", "))
    };
    conn.execute(
        &format!("DELETE FROM aave_v3_emode_categories WHERE market_id = ?1{predicate}"),
        params![market_id],
    )?;
    Ok(())
}

/// Write the restored reserve configuration — the generation facts a resumed
/// reduction reads to select an aToken/vToken movement rule, plus the settled
/// indices and rates the abandoned branch must not leave behind.
#[cfg(any(test, feature = "writer"))]
fn restore_assets(
    conn: &Connection,
    market_id: i64,
    assets: &[CheckpointAsset],
) -> Result<(), ProjectionError> {
    for asset in assets {
        let written = conn.execute(
            "UPDATE aave_v3_assets SET a_token_revision = ?3, v_token_revision = ?4, \
                 e_mode_category_id = ?5, price_source = ?6, last_update_block = ?7, \
                 liquidity_index = ?8, liquidity_rate = ?9, borrow_index = ?10, borrow_rate = ?11 \
             WHERE id = ?1 AND market_id = ?2",
            params![
                asset.asset_id,
                market_id,
                i64::from(asset.a_token_revision),
                i64::from(asset.v_token_revision),
                asset.e_mode_category_id,
                asset.price_source,
                asset
                    .last_update_block
                    .map(|block| i64::try_from(block).unwrap_or(i64::MAX)),
                asset.liquidity_index.to_string(),
                asset.liquidity_rate.to_string(),
                asset.borrow_index.to_string(),
                asset.borrow_rate.to_string(),
            ],
        )?;
        if written != 1 {
            return Err(ProjectionError::RecoveryAssetNotInMarket {
                market_id,
                asset_id: asset.asset_id,
            });
        }
    }
    Ok(())
}
