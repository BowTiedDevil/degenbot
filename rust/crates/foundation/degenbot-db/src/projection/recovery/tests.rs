//! Red-first acceptance for the retained-window reads and the market-scoped
//! restore a reorg recovery commits (ADR-105 D9): the cursor rewind is fenced by
//! the cursor the caller read, evidence above the common ancestor is pruned, the
//! state the checkpoint states is written back, a deeper recovery is refused
//! without moving anything, and one market's recovery leaves another byte-identical.

use alloy::primitives::{B256, U256};
use degenbot_core::canonical_head::ChainAnchor;
use rusqlite::params;

use super::{
    read_retained_checkpoints_on_conn, read_retained_journal_on_conn, restore_projection_on_conn,
    ProjectionRecovery, RecoveryRestore,
};
use crate::connection::DegenbotDb;
use crate::projection::{
    read_projection_cursor_on_conn, write_projection_unit_on_conn, CheckpointPositionState,
    CursorAnchor, DeltaJournalEntry, LogCoordinate, ProjectionCheckpoint, ProjectionCursor,
    ProjectionError, ProjectionUnit,
};
use crate::schema::ScaledTokenPosition;

const MARKET: i64 = 1;
const OTHER_MARKET: i64 = 2;

fn block_hash(branch: u8, number: u64) -> B256 {
    let tag = u64::from(branch) << 40;
    B256::from(U256::from(number | tag).to_be_bytes::<32>())
}

fn anchor(branch: u8, number: u64) -> ChainAnchor {
    ChainAnchor::new(
        1,
        number,
        block_hash(branch, number),
        block_hash(branch, number - 1),
    )
    .unwrap()
}

fn position(branch: u8, number: u64, log_index: u64) -> LogCoordinate {
    let tx_hash = B256::from(U256::from(0x1000_u64 + number).to_be_bytes::<32>());
    LogCoordinate::new(anchor(branch, number), tx_hash, 0, log_index)
}

fn market(db: &DegenbotDb, id: i64, numeric: Option<i64>) {
    db.lock()
        .execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
             VALUES (?1, 1, 'market', 1, ?2)",
            params![id, numeric],
        )
        .unwrap();
}

fn commit(
    db: &DegenbotDb,
    market_id: i64,
    position: LogCoordinate,
    anchor: CursorAnchor,
    settled: &[CheckpointPositionState],
    checkpoint: bool,
) -> ProjectionCursor {
    let entries = [DeltaJournalEntry {
        position,
        generation: None,
        encoding: format!("test-delta@{}", position.log_index()),
    }];
    let wire = ProjectionCheckpoint {
        encoding_version: "test/checkpoint",
        generations: &[],
        positions: settled,
        contracts: &[],
        gho: None,
    };
    let unit = ProjectionUnit {
        market_id,
        chain_id: 1,
        encoding_version: "test/delta",
        entries: &entries,
        range_end_block: position.anchor().number(),
        anchor,
        checkpoint: checkpoint.then_some(wire),
    };
    let conn = db.lock();
    let tx = conn.unchecked_transaction().unwrap();
    let outcome = write_projection_unit_on_conn(&tx, &unit).unwrap();
    tx.commit().unwrap();
    outcome.expect("a journaled unit establishes or advances the cursor")
}

fn cursor_of(db: &DegenbotDb, market_id: i64) -> Option<ProjectionCursor> {
    read_projection_cursor_on_conn(&db.lock(), market_id).unwrap()
}

fn journal_len(db: &DegenbotDb, market_id: i64) -> usize {
    read_retained_journal_on_conn(&db.lock(), market_id, 0)
        .unwrap()
        .len()
}

fn checkpoint_count(db: &DegenbotDb, market_id: i64) -> i64 {
    db.lock()
        .query_row(
            "SELECT COUNT(*) FROM aave_v3_projection_checkpoints WHERE market_id = ?1",
            params![market_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn checkpoint_position_rows(db: &DegenbotDb) -> i64 {
    db.lock()
        .query_row(
            "SELECT COUNT(*) FROM aave_v3_checkpoint_positions",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn settled(position: ScaledTokenPosition, id: i64, balance: u64) -> CheckpointPositionState {
    CheckpointPositionState {
        position,
        position_id: id,
        balance: U256::from(balance),
        index: Some(U256::from(1_u64)),
    }
}

fn numeric_cursor(db: &DegenbotDb, market_id: i64) -> Option<i64> {
    db.lock()
        .query_row(
            "SELECT last_update_block FROM aave_v3_markets WHERE id = ?1",
            params![market_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn balance_of(db: &DegenbotDb, position: ScaledTokenPosition, id: i64) -> Option<String> {
    db.lock()
        .query_row(
            &format!("SELECT balance FROM {} WHERE id = ?1", position.table()),
            params![id],
            |row| row.get(0),
        )
        .ok()
}

fn user(db: &DegenbotDb, id: i64, market_id: i64) {
    db.lock()
        .execute(
            "INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, \
             stk_aave_balance, isolation_mode_collateral_asset_id, isolation_mode_debt) \
             VALUES (?1, ?2, ?3, 0, 0, NULL, NULL, '0')",
            params![id, market_id, format!("0xuser{id}")],
        )
        .unwrap();
}

fn asset(db: &DegenbotDb, id: i64, market_id: i64, token_base: i64) {
    let conn = db.lock();
    for offset in 0..3 {
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
            params![token_base + offset, format!("0xtok{token_base}-{offset}")],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, a_token_id, \
         a_token_revision, v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
         borrow_index, borrow_rate) VALUES (?1, ?2, ?3, ?4, 1, ?5, 1, '0', '0', '1', '0')",
        params![id, market_id, token_base, token_base + 1, token_base + 2],
    )
    .unwrap();
}

fn collateral_position(db: &DegenbotDb, id: i64, user_id: i64, asset_id: i64, balance: &str) {
    db.lock()
        .execute(
            "INSERT INTO aave_v3_collateral_positions (id, user_id, asset_id, balance, last_index) \
             VALUES (?1, ?2, ?3, ?4, NULL)",
            params![id, user_id, asset_id, balance],
        )
        .unwrap();
}

fn recovered_market(branch: u8) -> (DegenbotDb, ProjectionCursor, Vec<CheckpointPositionState>) {
    let (db, _) = DegenbotDb::open_in_memory_for_writes().unwrap();
    market(&db, MARKET, Some(99));
    asset(&db, 1, MARKET, 1);
    user(&db, 1, MARKET);
    collateral_position(&db, 1, 1, 1, "500");
    let settled_state = vec![settled(ScaledTokenPosition::Collateral, 1, 500)];
    let mut cursor = commit(
        &db,
        MARKET,
        position(branch, 100, 1),
        CursorAnchor::First {
            replaces_numeric_cursor: Some(99),
        },
        &settled_state,
        true,
    );
    for number in [101_u64, 102] {
        cursor = commit(
            &db,
            MARKET,
            position(branch, number, 1),
            CursorAnchor::At(cursor),
            &settled_state,
            true,
        );
    }
    (db, cursor, settled_state)
}

fn recovery(
    expected: ProjectionCursor,
    ancestor_sequence: i64,
    restore: RecoveryRestore<'_>,
) -> ProjectionRecovery<'_> {
    ProjectionRecovery {
        market_id: MARKET,
        expected,
        ancestor: position(0xaa, 100, 1),
        ancestor_sequence,
        range_end_block: 100,
        restore,
    }
}

#[test]
fn a_recovery_rewinds_the_cursor_and_prunes_the_abandoned_evidence() {
    let (db, cursor, settled_state) = recovered_market(0xaa);
    assert_eq!(journal_len(&db, MARKET), 3);
    assert_eq!(checkpoint_count(&db, MARKET), 3);

    let restored = RecoveryRestore {
        positions: &settled_state,
        ..Default::default()
    };
    let rewound = {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let rewound = restore_projection_on_conn(&tx, &recovery(cursor, 1, restored)).unwrap();
        tx.commit().unwrap();
        rewound
    };

    assert_eq!(rewound.position, position(0xaa, 100, 1));
    assert_eq!(rewound.journal_sequence, 1);
    assert_eq!(rewound.range_end_block, 100);
    assert_eq!(cursor_of(&db, MARKET), Some(rewound));
    assert_eq!(
        numeric_cursor(&db, MARKET),
        Some(100),
        "the numeric range claim rewinds with the exact cursor, so the next run resumes at the ancestor"
    );
    assert_eq!(
        journal_len(&db, MARKET),
        1,
        "the abandoned branch's deltas are pruned, never replayed backwards"
    );
    assert_eq!(
        checkpoint_count(&db, MARKET),
        1,
        "the abandoned branch's checkpoints are pruned with it"
    );
    assert_eq!(
        checkpoint_position_rows(&db),
        1,
        "only the retained checkpoint's child rows survive"
    );
    let retained = read_retained_checkpoints_on_conn(&db.lock(), MARKET).unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].journal_sequence, 1);
    assert_eq!(retained[0].position, position(0xaa, 100, 1));
}

#[test]
fn a_recovery_beyond_the_retained_journal_is_refused_without_moving_anything() {
    let (db, cursor, settled_state) = recovered_market(0xaa);
    let before = (
        cursor_of(&db, MARKET),
        journal_len(&db, MARKET),
        checkpoint_count(&db, MARKET),
    );
    let restored = RecoveryRestore {
        positions: &settled_state,
        ..Default::default()
    };
    let error = {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let error = restore_projection_on_conn(&tx, &recovery(cursor, 0, restored)).unwrap_err();
        drop(tx);
        error
    };
    assert!(matches!(
        error,
        ProjectionError::RecoveryAncestorNotRetained { sequence: 0, .. }
    ));
    assert_eq!(
        (
            cursor_of(&db, MARKET),
            journal_len(&db, MARKET),
            checkpoint_count(&db, MARKET)
        ),
        before,
        "a deeper-than-retention recovery moves neither the cursor nor the evidence"
    );
}

#[test]
fn a_recovery_refuses_a_cursor_the_caller_did_not_read() {
    let (db, cursor, settled_state) = recovered_market(0xaa);
    let stale = ProjectionCursor {
        range_end_block: 101,
        ..cursor
    };
    let restored = RecoveryRestore {
        positions: &settled_state,
        ..Default::default()
    };
    let error = {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let error = restore_projection_on_conn(&tx, &recovery(stale, 1, restored)).unwrap_err();
        drop(tx);
        error
    };
    assert!(matches!(error, ProjectionError::UnexpectedParent { .. }));
    assert_eq!(cursor_of(&db, MARKET), Some(cursor));
    assert_eq!(journal_len(&db, MARKET), 3);
}

#[test]
fn a_recovery_restores_the_stated_positions_and_deletes_the_abandoned_rows() {
    let (db, cursor, _) = recovered_market(0xaa);
    {
        let conn = db.lock();
        conn.execute(
            "UPDATE aave_v3_collateral_positions SET balance = '999' WHERE id = 1",
            [],
        )
        .unwrap();
    }
    user(&db, 3, MARKET);
    collateral_position(&db, 2, 3, 1, "42");
    let restored_state = vec![settled(ScaledTokenPosition::Collateral, 1, 500)];
    let restored = RecoveryRestore {
        positions: &restored_state,
        ..Default::default()
    };
    {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        restore_projection_on_conn(&tx, &recovery(cursor, 1, restored)).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(
        balance_of(&db, ScaledTokenPosition::Collateral, 1).as_deref(),
        Some("500"),
        "the restored position state replaces the abandoned branch's balance"
    );
    assert_eq!(
        balance_of(&db, ScaledTokenPosition::Collateral, 2),
        None,
        "a position the ancestor's state does not hold is deleted"
    );
}

fn seed_reserve_config(db: &DegenbotDb, market_id: i64, asset_id: i64) {
    db.lock()
        .execute(
            "INSERT INTO aave_v3_emode_categories (id, market_id, category_id, label, ltv, \
             liquidation_threshold, liquidation_bonus, price_source) \
             VALUES (4, ?1, 4, 'e-mode', 900, 950, 1010, NULL)",
            params![market_id],
        )
        .unwrap();
    db.lock()
        .execute(
            "UPDATE aave_v3_assets SET price_source = '0xoracle', last_update_block = 500, \
             liquidity_index = '1001', liquidity_rate = '2', borrow_index = '1002', \
             borrow_rate = '3', e_mode_category_id = 4 WHERE id = ?1",
            params![asset_id],
        )
        .unwrap();
    db.lock()
        .execute(
            "INSERT INTO aave_v3_asset_configs (asset_id, ltv, liquidation_threshold, \
             liquidation_bonus, e_mode_category_id, borrowing_enabled, stable_borrowing_enabled, \
             flash_loan_enabled, isolation_mode, borrowable_in_isolation, debt_ceiling) \
             VALUES (?1, 800, 850, 1050, 4, 1, 0, 1, 0, 1, '77')",
            params![asset_id],
        )
        .unwrap();
    db.lock()
        .execute(
            "UPDATE aave_v3_users SET e_mode = 2, isolation_mode_collateral_asset_id = ?2, \
             isolation_mode_debt = '55' WHERE market_id = ?1",
            params![market_id, asset_id],
        )
        .unwrap();
}

/// The collateral flags and e-mode categories are resolution state, not derived
/// data: a checkpoint states them, and a rewind restores the stated values while
/// deleting what only the abandoned branch wrote.
#[test]
#[expect(clippy::too_many_lines)]
fn a_checkpoint_states_and_restores_the_collateral_flags_and_emode_categories() {
    let (db, cursor, _) = recovered_market(0xaa);
    asset(&db, 2, MARKET, 4);
    seed_reserve_config(&db, MARKET, 1);
    db.lock()
        .execute(
            "INSERT INTO aave_v3_user_collateral_configs (id, user_id, asset_id, enabled) \
             VALUES (1, 1, 1, 1)",
            [],
        )
        .unwrap();

    let rewound_cursor = {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let cursor = write_projection_unit_on_conn(
            &tx,
            &ProjectionUnit {
                market_id: MARKET,
                chain_id: 1,
                encoding_version: "test/delta",
                entries: &[crate::projection::DeltaJournalEntry {
                    position: position(0xaa, 103, 1),
                    generation: None,
                    encoding: "test-delta@1".to_string(),
                }],
                range_end_block: 103,
                anchor: CursorAnchor::At(cursor),
                checkpoint: Some(ProjectionCheckpoint {
                    encoding_version: "test/checkpoint",
                    generations: &[],
                    positions: &[],
                    contracts: &[],
                    gho: None,
                }),
            },
        )
        .unwrap()
        .expect("the unit advances the cursor");
        tx.commit().unwrap();
        cursor
    };

    let row = crate::projection::read_checkpoint_on_conn(&db.lock(), MARKET, 4)
        .unwrap()
        .expect("the checkpoint is retained");
    assert_eq!(
        row.user_collaterals,
        vec![crate::projection::CheckpointUserCollateral {
            user_id: 1,
            asset_id: 1,
            enabled: true,
        }],
        "the checkpoint states the collateral flags the cursor settled"
    );
    assert_eq!(
        row.emode_categories,
        vec![crate::projection::CheckpointEModeCategory {
            category_id: 4,
            label: Some("e-mode".to_string()),
            ltv: 900,
            liquidation_threshold: 950,
            liquidation_bonus: 1010,
            price_source: None,
        }],
        "the checkpoint states the category definitions in force"
    );

    {
        let conn = db.lock();
        conn.execute(
            "UPDATE aave_v3_user_collateral_configs SET enabled = 0 \
             WHERE user_id = 1 AND asset_id = 1",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_user_collateral_configs (id, user_id, asset_id, enabled) \
             VALUES (3, 1, 2, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE aave_v3_assets SET e_mode_category_id = NULL WHERE id = 1",
            [],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM aave_v3_emode_categories WHERE category_id = 4",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_emode_categories (id, market_id, category_id, label, ltv, \
             liquidation_threshold, liquidation_bonus, price_source) \
             VALUES (9, 1, 7, 'abandoned', 1, 2, 3, '0xstale')",
            [],
        )
        .unwrap();
    }

    {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        restore_projection_on_conn(
            &tx,
            &ProjectionRecovery {
                market_id: MARKET,
                expected: rewound_cursor,
                ancestor: position(0xaa, 103, 1),
                ancestor_sequence: 4,
                range_end_block: 103,
                restore: RecoveryRestore {
                    user_collaterals: &row.user_collaterals,
                    emode_categories: &row.emode_categories,
                    ..Default::default()
                },
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let enabled: Vec<(i64, bool)> = {
        let conn = db.lock();
        let mut statement = conn
            .prepare(
                "SELECT asset_id, enabled FROM aave_v3_user_collateral_configs \
                 WHERE user_id = 1 ORDER BY asset_id",
            )
            .unwrap();
        let states = statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        states
    };
    assert_eq!(
        enabled,
        vec![(1, true)],
        "the stated flag comes back and the abandoned enable is removed"
    );

    let categories: Vec<(i64, String)> = {
        let conn = db.lock();
        let mut statement = conn
            .prepare(
                "SELECT category_id, label FROM aave_v3_emode_categories WHERE market_id = 1 \
                 ORDER BY category_id",
            )
            .unwrap();
        let stated = statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        stated
    };
    assert_eq!(
        categories,
        vec![(4, "e-mode".to_string())],
        "the deleted category is restored and the abandoned definition is removed"
    );
}

/// The user and reserve configuration families a checkpoint states are restored
/// from the checkpoint alone: the abandoned branch's e-mode, isolation, indices,
/// rates, price source, update stamp, and configuration bitmap do not survive the
/// rewind.
#[test]
#[expect(clippy::too_many_lines)]
fn a_checkpoint_states_and_restores_the_user_and_reserve_configuration() {
    let (db, cursor, _) = recovered_market(0xaa);
    seed_reserve_config(&db, MARKET, 1);
    let rewound_cursor = {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let cursor = write_projection_unit_on_conn(
            &tx,
            &ProjectionUnit {
                market_id: MARKET,
                chain_id: 1,
                encoding_version: "test/delta",
                entries: &[crate::projection::DeltaJournalEntry {
                    position: position(0xaa, 103, 1),
                    generation: None,
                    encoding: "test-delta@1".to_string(),
                }],
                range_end_block: 103,
                anchor: CursorAnchor::At(cursor),
                checkpoint: Some(ProjectionCheckpoint {
                    encoding_version: "test/checkpoint",
                    generations: &[],
                    positions: &[],
                    contracts: &[],
                    gho: None,
                }),
            },
        )
        .unwrap()
        .expect("the unit advances the cursor");
        tx.commit().unwrap();
        cursor
    };
    assert_eq!(rewound_cursor.journal_sequence, 4);

    {
        let conn = db.lock();
        conn.execute(
            "UPDATE aave_v3_users SET e_mode = 9, isolation_mode_debt = '999', \
             isolation_mode_collateral_asset_id = NULL WHERE market_id = 1",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE aave_v3_assets SET liquidity_index = '424242', price_source = '0xstale', \
             last_update_block = 9999, borrow_rate = '888' WHERE id = 1",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE aave_v3_asset_configs SET ltv = 1, borrowing_enabled = 0 WHERE asset_id = 1",
            [],
        )
        .unwrap();
    }

    let row = crate::projection::read_checkpoint_on_conn(&db.lock(), MARKET, 4)
        .unwrap()
        .expect("the checkpoint is retained");
    {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        restore_projection_on_conn(
            &tx,
            &ProjectionRecovery {
                market_id: MARKET,
                expected: rewound_cursor,
                ancestor: position(0xaa, 103, 1),
                ancestor_sequence: 4,
                range_end_block: 103,
                restore: RecoveryRestore {
                    positions: &row.positions,
                    assets: &row.assets,
                    asset_configs: &row.asset_configs,
                    users: &row.users,
                    user_collaterals: &row.user_collaterals,
                    emode_categories: &row.emode_categories,
                    ..Default::default()
                },
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let (e_mode, isolation_debt, isolation_collateral) = db
        .lock()
        .query_row(
            "SELECT e_mode, isolation_mode_debt, isolation_mode_collateral_asset_id \
             FROM aave_v3_users WHERE market_id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        e_mode, 2,
        "the restored user's e-mode replaces the abandoned one"
    );
    assert_eq!(isolation_debt, "55", "the abandoned debt ceiling is gone");
    assert_eq!(isolation_collateral, Some(1));

    let (li, rate, price, last, category) = db
        .lock()
        .query_row(
            "SELECT liquidity_index, borrow_rate, price_source, last_update_block, \
             e_mode_category_id FROM aave_v3_assets WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(li, "1001", "the abandoned index does not survive recovery");
    assert_eq!(rate, "3", "the abandoned borrow rate is replaced");
    assert_eq!(price.as_deref(), Some("0xoracle"));
    assert_eq!(last, Some(500));
    assert_eq!(category, Some(4));

    let (ltv, borrowing) = db
        .lock()
        .query_row(
            "SELECT ltv, borrowing_enabled FROM aave_v3_asset_configs WHERE asset_id = 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
        )
        .unwrap();
    assert_eq!(ltv, 800, "the bitmap-decoded configuration is restored");
    assert!(borrowing, "the abandoned disabled flag is replaced");
}

/// A recovery is scoped to one market: a second market sharing the chain keeps
/// its cursor, journal, and restored rows untouched (ADR-105 D9's per-market
/// scope).
#[test]
fn a_recovery_leaves_another_market_untouched() {
    let (db, cursor, settled_state) = recovered_market(0xaa);
    market(&db, OTHER_MARKET, Some(199));
    let other_cursor = commit(
        &db,
        OTHER_MARKET,
        position(0xcc, 200, 1),
        CursorAnchor::First {
            replaces_numeric_cursor: Some(199),
        },
        &[],
        true,
    );
    let other_journal = journal_len(&db, OTHER_MARKET);
    let other_checkpoints = checkpoint_count(&db, OTHER_MARKET);
    let other_numeric = numeric_cursor(&db, OTHER_MARKET);

    let restored = RecoveryRestore {
        positions: &settled_state,
        ..Default::default()
    };
    {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        restore_projection_on_conn(&tx, &recovery(cursor, 1, restored)).unwrap();
        tx.commit().unwrap();
    }

    assert_eq!(
        cursor_of(&db, MARKET).map(|held| held.journal_sequence),
        Some(1)
    );
    assert_eq!(cursor_of(&db, OTHER_MARKET), Some(other_cursor));
    assert_eq!(journal_len(&db, OTHER_MARKET), other_journal);
    assert_eq!(checkpoint_count(&db, OTHER_MARKET), other_checkpoints);
    assert_eq!(numeric_cursor(&db, OTHER_MARKET), other_numeric);
}
