//! Red-first acceptance for bounded checkpoint replay and reorg recovery
//! (ADR-105 D9): same-height alternative branches, exact N+1..M replay, competing
//! branch activations, retention overflow, multi-market isolation, and the stale
//! Epoch / expected-parent alignment.
use alloy::primitives::{B256, I256, U256};
use degenbot_core::canonical_head::{ChainAnchor, LogRef, TxRef};
use degenbot_db::{
    read_projection_cursor_on_conn, read_retained_journal_on_conn, CursorAnchor, DegenbotDb,
    ProjectionCursor, ScaledTokenPosition,
};
use rusqlite::params;

use super::{
    plan_recovery, read_retained_evidence, recover_on_conn, CanonicalSegment, Recovery,
    RecoveryError,
};
use crate::projection::{
    journal_entries, CanonicalDelta, CheckpointState, ConfigurationEffect, DeltaEffect,
    GenerationActivation, GenerationEntity, GenerationRef, PositionSubject, ProjectionError,
    ProjectionView, DELTA_ENCODING_VERSION,
};
use crate::updater::run::substrate::ChunkSubstrate;

const MARKET: i64 = 1;
const OTHER_MARKET: i64 = 2;
const RAY: u128 = 1_000_000_000_000_000_000_000_000_000;

fn block_hash(branch: u8, number: u64) -> B256 {
    let tag = u64::from(branch) << 40;
    B256::from(U256::from(number | tag).to_be_bytes::<32>())
}

/// One branch of headers, linked parent-to-child: the evidence a caller acquires
/// before it asks for recovery.
#[derive(Default)]
struct Chain {
    anchors: Vec<ChainAnchor>,
}

impl Chain {
    fn push(&mut self, branch: u8, number: u64) -> ChainAnchor {
        let parent = self
            .anchors
            .last()
            .map_or_else(|| block_hash(branch, number - 1), ChainAnchor::hash);
        let anchor = ChainAnchor::new(1, number, block_hash(branch, number), parent).unwrap();
        self.anchors.push(anchor);
        anchor
    }

    fn holds(&self, number: u64) -> ChainAnchor {
        *self
            .anchors
            .iter()
            .find(|anchor| anchor.number() == number)
            .expect("the chain holds the height")
    }

    fn segment(&self) -> CanonicalSegment {
        CanonicalSegment::new(self.anchors.clone()).unwrap()
    }

    fn log(&self, number: u64, log_index: u64) -> LogRef {
        let anchor = self.holds(number);
        let tag = (0x1000_u64 + number).to_be_bytes();
        let mut material = anchor.hash().to_vec();
        material.extend_from_slice(&tag);
        let tx_hash = alloy::primitives::keccak256(&material);
        LogRef::new(TxRef::new(anchor, tx_hash, 0).unwrap(), log_index)
    }
}

fn collateral(position_id: i64) -> PositionSubject {
    PositionSubject {
        position: ScaledTokenPosition::Collateral,
        position_id,
    }
}

fn market(db: &DegenbotDb, id: i64, numeric: i64) {
    db.lock()
        .execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
             VALUES (?1, 1, 'market', 1, ?2)",
            params![id, numeric],
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

fn collateral_position(db: &DegenbotDb, id: i64, user_id: i64, asset_id: i64, balance: &str) {
    db.lock()
        .execute(
            "INSERT INTO aave_v3_collateral_positions (id, user_id, asset_id, balance, last_index) \
             VALUES (?1, ?2, ?3, ?4, NULL)",
            params![id, user_id, asset_id, balance],
        )
        .unwrap();
}

/// A market with one reserve, one user, and one settled collateral position — the
/// relational parents a checkpoint's position family needs.
fn seeded_market(
    db: &DegenbotDb,
    id: i64,
    numeric: i64,
    tokens: i64,
    position_id: i64,
    assets: i64,
) {
    market(db, id, numeric);
    asset(db, assets, id, tokens);
    user(db, position_id, id);
    collateral_position(db, position_id, position_id, assets, "500");
}

fn coordinate(at: LogRef) -> degenbot_db::LogCoordinate {
    degenbot_db::LogCoordinate::new(
        *at.tx().anchor(),
        at.tx().hash(),
        at.tx().index(),
        at.index(),
    )
}

fn balance_delta(source: LogRef, amount: i64, subject: PositionSubject) -> CanonicalDelta {
    CanonicalDelta::new(
        source,
        None,
        DeltaEffect::ScaledBalance {
            subject,
            balance_delta: I256::try_from(amount).unwrap(),
            index: U256::from(RAY),
        },
    )
}

/// The resume state a reduction at a unit boundary holds: the settled positions
/// (and generations) the view carries.
fn checkpoint_state(
    seeded: &[(PositionSubject, (U256, Option<U256>))],
    generations: &[GenerationRef],
) -> CheckpointState {
    let mut view = ProjectionView::new();
    for (subject, state) in seeded {
        view.seed_position(*subject, *state);
    }
    for generation in generations {
        view.observe_generation(*generation);
    }
    CheckpointState::capture(&view, &ChunkSubstrate::lazy(1))
}

fn commit(
    db: &DegenbotDb,
    market_id: i64,
    anchor: CursorAnchor,
    deltas: &[CanonicalDelta],
    checkpoint: Option<&CheckpointState>,
) -> ProjectionCursor {
    let entries = journal_entries(deltas);
    let range_end = deltas
        .last()
        .map_or(1, |delta| delta.source().tx().anchor().number());
    let unit = degenbot_db::ProjectionUnit {
        market_id,
        chain_id: 1,
        encoding_version: DELTA_ENCODING_VERSION,
        entries: &entries,
        range_end_block: range_end,
        anchor,
        checkpoint: checkpoint.map(CheckpointState::wire),
    };
    let conn = db.lock();
    let tx = conn.unchecked_transaction().unwrap();
    let outcome = degenbot_db::write_projection_unit_on_conn(&tx, &unit).unwrap();
    tx.commit().unwrap();
    outcome.expect("a journaled unit advances the cursor")
}

fn cursor_of(db: &DegenbotDb, market_id: i64) -> Option<ProjectionCursor> {
    read_projection_cursor_on_conn(&db.lock(), market_id).unwrap()
}

fn journal_len(db: &DegenbotDb, market_id: i64) -> usize {
    read_retained_journal_on_conn(&db.lock(), market_id, 0)
        .unwrap()
        .len()
}

#[test]
fn a_head_that_continues_the_cursor_needs_no_recovery() {
    let (db, _) = DegenbotDb::open_in_memory_for_writes().unwrap();
    seeded_market(&db, MARKET, 99, 1, 1, 1);
    let mut chain = Chain::default();
    chain.push(0xaa, 100);
    let state = checkpoint_state(&[(collateral(1), (U256::from(500_u64), None))], &[]);
    let cursor = commit(
        &db,
        MARKET,
        CursorAnchor::First {
            replaces_numeric_cursor: Some(99),
        },
        &[balance_delta(chain.log(100, 1), 0, collateral(1))],
        Some(&state),
    );
    chain.push(0xaa, 101);
    let version = chain.segment();

    let recovered = {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let recovered = recover_on_conn(&tx, MARKET, &version).unwrap();
        tx.commit().unwrap();
        recovered
    };
    assert!(matches!(recovered, Recovery::Continues));
    assert_eq!(cursor_of(&db, MARKET), Some(cursor));
    assert_eq!(journal_len(&db, MARKET), 1);
}

#[test]
fn same_height_alternative_branches_restore_to_the_shared_ancestor() {
    let (db, _) = DegenbotDb::open_in_memory_for_writes().unwrap();
    seeded_market(&db, MARKET, 99, 1, 1, 1);
    let mut abandoned = Chain::default();
    abandoned.push(0xaa, 100);
    let state = checkpoint_state(&[(collateral(1), (U256::from(500_u64), None))], &[]);
    let mut cursor = commit(
        &db,
        MARKET,
        CursorAnchor::First {
            replaces_numeric_cursor: Some(99),
        },
        &[balance_delta(abandoned.log(100, 1), 100, collateral(1))],
        Some(&state),
    );
    for number in [101_u64, 102] {
        abandoned.push(0xaa, number);
        cursor = commit(
            &db,
            MARKET,
            CursorAnchor::At(cursor),
            &[balance_delta(abandoned.log(number, 1), 10, collateral(1))],
            None,
        );
    }
    let abandoned_cursor = cursor;

    let mut alternative = Chain::default();
    alternative.push(0xaa, 100);
    alternative.push(0xbb, 101);
    alternative.push(0xbb, 102);
    let version = alternative.segment();

    let recovered = {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let recovered = recover_on_conn(&tx, MARKET, &version).unwrap();
        tx.commit().unwrap();
        recovered
    };
    let Recovery::Restored {
        cursor,
        view,
        ancestor,
        replayed,
    } = recovered
    else {
        panic!("a same-height alternative branch must recover, not continue");
    };
    assert_eq!(ancestor, coordinate(abandoned.log(100, 1)));
    assert_eq!(ancestor.anchor().hash(), block_hash(0xaa, 100));
    assert_eq!(
        replayed, 0,
        "the shared block's checkpoint is the restore point"
    );
    assert_eq!(cursor.journal_sequence, 1);
    assert_eq!(view.cursor(), Some(abandoned.log(100, 1)));
    assert_eq!(
        journal_len(&db, MARKET),
        1,
        "only the shared branch's journal survives the rewind"
    );

    let new_cursor = commit(
        &db,
        MARKET,
        CursorAnchor::At(cursor),
        &[balance_delta(alternative.log(101, 1), 7, collateral(1))],
        None,
    );
    let new_cursor = commit(
        &db,
        MARKET,
        CursorAnchor::At(new_cursor),
        &[balance_delta(alternative.log(102, 1), 3, collateral(1))],
        None,
    );
    assert_eq!(
        new_cursor.position.anchor().hash(),
        block_hash(0xbb, 102),
        "the recovered market resumes on the new branch"
    );
    assert_ne!(
        new_cursor.position, abandoned_cursor.position,
        "the abandoned cursor's exact position is never reused"
    );
}

#[test]
fn recovery_replays_the_retained_deltas_forward_exactly_once() {
    let (db, _) = DegenbotDb::open_in_memory_for_writes().unwrap();
    seeded_market(&db, MARKET, 99, 1, 1, 1);
    let mut chain = Chain::default();
    chain.push(0xaa, 100);
    db.lock()
        .execute(
            "UPDATE aave_v3_collateral_positions SET balance = '600' WHERE id = 1",
            [],
        )
        .unwrap();
    let state = checkpoint_state(
        &[(collateral(1), (U256::from(600_u64), Some(U256::from(RAY))))],
        &[],
    );
    let mut cursor = commit(
        &db,
        MARKET,
        CursorAnchor::First {
            replaces_numeric_cursor: Some(99),
        },
        &[balance_delta(chain.log(100, 1), 100, collateral(1))],
        Some(&state),
    );
    let mut retained = Vec::new();
    for (number, amount) in [(101_u64, 1_000_i64), (102, -250), (103, 7)] {
        chain.push(0xaa, number);
        let delta = balance_delta(chain.log(number, 1), amount, collateral(1));
        if number < 103 {
            retained.push(delta.clone());
        }
        cursor = commit(&db, MARKET, CursorAnchor::At(cursor), &[delta], None);
    }
    let mut version_chain = Chain::default();
    for number in [100_u64, 101, 102] {
        version_chain.push(0xaa, number);
    }
    version_chain.push(0xbb, 103);
    let version = version_chain.segment();

    let recovered = {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let recovered = recover_on_conn(&tx, MARKET, &version).unwrap();
        tx.commit().unwrap();
        recovered
    };
    let Recovery::Restored {
        cursor,
        view,
        ancestor,
        replayed,
    } = recovered
    else {
        panic!("a reorg above the cursor must recover");
    };
    assert_eq!(replayed, 2, "deltas N+1..M replay in one forward pass");
    assert_eq!(ancestor, coordinate(chain.log(102, 1)));
    assert_eq!(cursor.journal_sequence, 3);

    let mut clean = ProjectionView::new();
    clean.seed_position(collateral(1), (U256::from(600_u64), Some(U256::from(RAY))));
    for delta in &retained {
        clean.apply(delta).unwrap();
    }
    assert_eq!(
        view.position_state(collateral(1)),
        clean.position_state(collateral(1)),
        "the replayed projection matches a clean forward projection"
    );
    assert_eq!(
        view.position_state(collateral(1)),
        Some((U256::from(1_350_u64), Some(U256::from(RAY)))),
        "every retained movement applied exactly once"
    );
    assert_eq!(view.cursor(), clean.cursor());
    assert_eq!(
        journal_len(&db, MARKET),
        3,
        "the shared branch's retained deltas stay journaled"
    );
}

#[test]
fn a_reorg_deeper_than_the_retained_evidence_is_refused_without_moving_anything() {
    let (db, _) = DegenbotDb::open_in_memory_for_writes().unwrap();
    seeded_market(&db, MARKET, 99, 1, 1, 1);
    let mut chain = Chain::default();
    chain.push(0xaa, 100);
    let state = checkpoint_state(&[(collateral(1), (U256::from(500_u64), None))], &[]);
    let cursor = commit(
        &db,
        MARKET,
        CursorAnchor::First {
            replaces_numeric_cursor: Some(99),
        },
        &[balance_delta(chain.log(100, 1), 0, collateral(1))],
        Some(&state),
    );
    let before = (cursor, journal_len(&db, MARKET));

    let mut deep = Chain::default();
    deep.push(0xbb, 200);
    deep.push(0xbb, 201);
    let version = deep.segment();
    let error = {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let error = recover_on_conn(&tx, MARKET, &version).unwrap_err();
        drop(tx);
        error
    };
    match &error {
        RecoveryError::Projection(ProjectionError::RetentionExceeded {
            retained_from,
            covered_from,
            deepest_checkpoint,
            ..
        }) => {
            assert_eq!(*retained_from, 100);
            assert_eq!(*covered_from, 200);
            assert_eq!(*deepest_checkpoint, Some(1));
        }
        other => panic!("a deeper reorg must name the retention overflow, got {other:?}"),
    }
    assert!(
        error.to_string().contains("market reset"),
        "the refusal names the recovery path: {error}"
    );
    assert_eq!(
        (cursor_of(&db, MARKET), journal_len(&db, MARKET)),
        (Some(before.0), before.1),
        "a refused recovery moves no cursor and restores nothing"
    );

    let evidence = {
        let conn = db.lock();
        read_retained_evidence(&conn, MARKET).unwrap().unwrap()
    };
    assert!(matches!(
        plan_recovery(&evidence, &version),
        Err(ProjectionError::RetentionExceeded { .. })
    ));
}

#[test]
fn competing_branch_activations_keep_distinct_generations() {
    let (db, _) = DegenbotDb::open_in_memory_for_writes().unwrap();
    seeded_market(&db, MARKET, 99, 1, 1, 1);
    let entity = GenerationEntity::Contract {
        market_id: MARKET,
        role: crate::projection::ContractRole::Pool,
    };
    let mut chain = Chain::default();
    chain.push(0xaa, 100);
    let state = checkpoint_state(
        &[(collateral(1), (U256::from(500_u64), None))],
        &[GenerationRef::new(entity, 1)],
    );
    let cursor = commit(
        &db,
        MARKET,
        CursorAnchor::First {
            replaces_numeric_cursor: Some(99),
        },
        &[balance_delta(chain.log(100, 1), 0, collateral(1))],
        Some(&state),
    );
    chain.push(0xaa, 101);
    let abandoned_activation = chain.log(101, 1);
    let upgrade = CanonicalDelta::new(
        abandoned_activation,
        Some(GenerationRef::activated_at(
            entity,
            Some(2),
            abandoned_activation,
        )),
        DeltaEffect::Configuration(ConfigurationEffect::ContractRevisionUpdated {
            market_id: MARKET,
            contract_name: "POOL".to_string(),
            new_revision: 2,
        }),
    );
    commit(&db, MARKET, CursorAnchor::At(cursor), &[upgrade], None);

    let mut alternative = Chain::default();
    alternative.push(0xaa, 100);
    alternative.push(0xbb, 101);
    alternative.push(0xbb, 102);
    let version = alternative.segment();
    let recovered = {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let recovered = recover_on_conn(&tx, MARKET, &version).unwrap();
        tx.commit().unwrap();
        recovered
    };
    let Recovery::Restored { mut view, .. } = recovered else {
        panic!("the abandoned activation must be rolled back to the shared ancestor");
    };
    assert_eq!(
        view.generation(entity)
            .and_then(|generation| generation.revision()),
        Some(1),
        "the restored projection reads the shared generation, not the abandoned one"
    );
    let replacement = alternative.log(101, 1);
    let activating = CanonicalDelta::new(
        replacement,
        Some(GenerationRef::activated_at(entity, Some(3), replacement)),
        DeltaEffect::Configuration(ConfigurationEffect::ContractRevisionUpdated {
            market_id: MARKET,
            contract_name: "POOL".to_string(),
            new_revision: 3,
        }),
    );
    view.apply(&activating).unwrap();
    let installed = view
        .generation(entity)
        .expect("the view reads the new generation");
    assert_eq!(installed.revision(), Some(3));
    assert_eq!(
        installed.activation(),
        GenerationActivation::Event(replacement),
        "the competing branch's activation is its own event, never the abandoned one"
    );
    assert_ne!(
        installed.activation(),
        GenerationActivation::Event(abandoned_activation)
    );

    let replaced_generation = CanonicalDelta::new(
        alternative.log(102, 1),
        Some(GenerationRef::new(entity, 2)),
        DeltaEffect::GhoRefreshDiscount { position_id: 1 },
    );
    assert!(matches!(
        view.apply(&replaced_generation),
        Err(ProjectionError::StaleGeneration { .. })
    ));
}

#[test]
fn the_session_reorg_window_bounds_the_retained_evidence() {
    let (db, _) = DegenbotDb::open_in_memory_for_writes().unwrap();
    seeded_market(&db, MARKET, 99, 1, 1, 1);
    let state = checkpoint_state(&[(collateral(1), (U256::from(500_u64), None))], &[]);
    let mut chain = Chain::default();
    let mut cursor = None;
    for (index, number) in (100_u64..=111).enumerate() {
        chain.push(0xaa, number);
        let anchor = cursor.map_or(
            CursorAnchor::First {
                replaces_numeric_cursor: Some(99),
            },
            CursorAnchor::At,
        );
        cursor = Some(commit(
            &db,
            MARKET,
            anchor,
            &[balance_delta(
                chain.log(number, 1),
                i64::try_from(index).unwrap(),
                collateral(1),
            )],
            Some(&state),
        ));
    }
    let cursor = cursor.expect("the seeded run commits");

    let retained = read_retained_journal_on_conn(&db.lock(), MARKET, 0).unwrap();
    assert_eq!(
        retained.len(),
        usize::try_from(degenbot_db::REORG_WINDOW_BLOCKS).unwrap(),
        "the journal keeps exactly the session's reorg window"
    );
    assert_eq!(
        retained.first().unwrap().position.anchor().number(),
        111 - degenbot_db::REORG_WINDOW_BLOCKS + 1,
        "the retained window's floor follows the committed head"
    );
    let checkpoints = degenbot_db::read_retained_checkpoints_on_conn(&db.lock(), MARKET).unwrap();
    assert_eq!(
        checkpoints.len(),
        retained.len(),
        "checkpoints prune with the journal"
    );
    assert_eq!(
        checkpoints.first().unwrap().journal_sequence,
        retained.first().unwrap().sequence,
        "the oldest retained row keeps the checkpoint a recovery restores from"
    );

    let mut inside = Chain::default();
    for number in 100_u64..=106 {
        inside.push(0xaa, number);
    }
    inside.push(0xbb, 107);
    let recovered = {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let recovered = recover_on_conn(&tx, MARKET, &inside.segment()).unwrap();
        tx.commit().unwrap();
        recovered
    };
    let Recovery::Restored { ancestor, .. } = recovered else {
        panic!("a reorg inside the retained window must recover");
    };
    assert_eq!(ancestor.anchor().number(), 106);
    let _ = cursor;

    let mut deep = Chain::default();
    deep.push(0xbb, 200);
    deep.push(0xbb, 201);
    let error = {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        let error = recover_on_conn(&tx, MARKET, &deep.segment()).unwrap_err();
        drop(tx);
        error
    };
    assert!(
        matches!(
            error,
            RecoveryError::Projection(ProjectionError::RetentionExceeded { .. })
        ),
        "a reorg deeper than the window is an explicit refusal, got {error:?}"
    );
}

#[test]
fn one_markets_recovery_leaves_another_market_untouched() {
    let (db, _) = DegenbotDb::open_in_memory_for_writes().unwrap();
    seeded_market(&db, MARKET, 99, 1, 1, 1);
    seeded_market(&db, OTHER_MARKET, 199, 4, 2, 2);
    let mut chain = Chain::default();
    chain.push(0xaa, 100);
    let state = checkpoint_state(&[(collateral(1), (U256::from(500_u64), None))], &[]);
    let cursor = commit(
        &db,
        MARKET,
        CursorAnchor::First {
            replaces_numeric_cursor: Some(99),
        },
        &[balance_delta(chain.log(100, 1), 0, collateral(1))],
        Some(&state),
    );
    chain.push(0xaa, 101);
    commit(
        &db,
        MARKET,
        CursorAnchor::At(cursor),
        &[balance_delta(chain.log(101, 1), 5, collateral(1))],
        None,
    );

    let mut other_chain = Chain::default();
    other_chain.push(0xcc, 200);
    let other_state = checkpoint_state(&[(collateral(2), (U256::from(7_u64), None))], &[]);
    let other_cursor = commit(
        &db,
        OTHER_MARKET,
        CursorAnchor::First {
            replaces_numeric_cursor: Some(199),
        },
        &[balance_delta(other_chain.log(200, 1), 0, collateral(2))],
        Some(&other_state),
    );
    let subject = (
        other_cursor,
        journal_len(&db, OTHER_MARKET),
        degenbot_db::read_retained_checkpoints_on_conn(&db.lock(), OTHER_MARKET)
            .unwrap()
            .len(),
    );

    let mut alternative = Chain::default();
    alternative.push(0xaa, 100);
    alternative.push(0xbb, 101);
    let version = alternative.segment();
    {
        let conn = db.lock();
        let tx = conn.unchecked_transaction().unwrap();
        recover_on_conn(&tx, MARKET, &version).unwrap();
        tx.commit().unwrap();
    }

    assert_eq!(
        (
            cursor_of(&db, OTHER_MARKET).unwrap(),
            journal_len(&db, OTHER_MARKET),
            degenbot_db::read_retained_checkpoints_on_conn(&db.lock(), OTHER_MARKET)
                .unwrap()
                .len(),
        ),
        subject,
        "recovering one market leaves every other market byte-identical"
    );
}
