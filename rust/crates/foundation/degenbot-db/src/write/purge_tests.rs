use super::*;
use crate::connection::DegenbotDb;

/// A write-capable in-memory DB carrying two markets. Market A (id 1) is
/// fully populated; market B (id 2) plus the non-market tables
/// (`erc20_tokens`, `pools`, `exchanges`) are populated too, so a purge of A
/// has something it must leave alone.
fn two_market_db() -> DegenbotDb {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    {
        let conn = db.lock();
        // Two markets on the same chain.
        for (id, name) in [(1_i64, "Market A"), (2, "Market B")] {
            conn.execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (?1, 1, ?2, 1, 500)",
                params![id, name],
            )
            .unwrap();
        }
        // A non-market table with rows keyed to neither market.
        conn.execute(
            "INSERT INTO exchanges (id, chain_id, name, active, factory) \
             VALUES (1, 1, 'uniswap_v3', 1, '0xfactory')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address, name, symbol, decimals) \
             VALUES (1, 1, '0xtok1', 'Tok', 'TOK', 18)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) \
             VALUES (1, '0xpool', 1, 'uniswap_v3', 1, 1, 1)",
            [],
        )
        .unwrap();
        // Per-market substrate + populated rows for BOTH markets.
        for market_id in [1_i64, 2] {
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &conn,
                market_id,
                "POOL_ADDRESS_PROVIDER",
                &format!("0xap{market_id}"),
                None,
            )
            .unwrap();
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &conn,
                market_id,
                "POOL",
                &format!("0xpool{market_id}"),
                Some(1),
            )
            .unwrap();
            let asset_id = DegenbotDb::apply_reserve_initialized_on_conn(
                &conn, market_id, 1, 1, 1, 1, 1, None, None,
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_users \
                 (market_id, address, e_mode, gho_discount, stk_aave_balance, \
                  isolation_mode_collateral_asset_id, isolation_mode_debt) \
                 VALUES (?1, ?2, 0, 0, NULL, NULL, '0')",
                params![market_id, format!("0xuser{market_id}")],
            )
            .unwrap();
            let user_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO aave_v3_collateral_positions (user_id, asset_id, balance, last_index) \
                 VALUES (?1, ?2, '10', '1')",
                params![user_id, asset_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_debt_positions (user_id, asset_id, balance, last_index) \
                 VALUES (?1, ?2, '5', '1')",
                params![user_id, asset_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_emode_categories \
                 (market_id, category_id, label, ltv, liquidation_threshold, liquidation_bonus) \
                 VALUES (?1, 1, 'e', 1, 2, 3)",
                params![market_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_asset_configs \
                 (asset_id, ltv, liquidation_threshold, liquidation_bonus, borrowing_enabled, \
                  stable_borrowing_enabled, flash_loan_enabled, isolation_mode, \
                  borrowable_in_isolation) \
                 VALUES (?1, 1, 2, 3, 1, 0, 1, 0, 0)",
                params![asset_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_user_collateral_configs (user_id, asset_id, enabled) \
                 VALUES (?1, ?2, 1)",
                params![user_id, asset_id],
            )
            .unwrap();
        }
    }
    db
}

/// The row counts of the tables a purge must never touch.
fn untouched_counts(db: &DegenbotDb) -> (i64, i64, i64) {
    let conn = db.lock();
    let count = |table: &str| -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    (count("erc20_tokens"), count("pools"), count("exchanges"))
}

/// The per-market row count of `table`, for the market-scoping assertions.
fn market_rows(db: &DegenbotDb, table: &str, market_id: i64) -> i64 {
    let conn = db.lock();
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE market_id = ?1"),
        params![market_id],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn count_reports_the_rows_a_purge_would_remove() {
    let db = two_market_db();
    let counts = db.count_aave_market_rows(1).unwrap();
    let by_table = |t: &str| counts.iter().find(|c| c.table == t).unwrap().rows;
    assert_eq!(by_table("aave_v3_collateral_positions"), 1);
    assert_eq!(by_table("aave_v3_debt_positions"), 1);
    assert_eq!(by_table("aave_v3_users"), 1);
    assert_eq!(by_table("aave_v3_assets"), 1);
    assert_eq!(by_table("aave_v3_emode_categories"), 1);
    assert_eq!(by_table("aave_v3_asset_configs"), 1);
    assert_eq!(by_table("aave_v3_user_collateral_configs"), 1);
    // The non-address-provider contract row only.
    assert_eq!(by_table("aave_v3_contracts"), 1);
}

#[test]
fn purge_clears_one_market_and_leaves_the_other_and_non_market_tables_untouched() {
    let db = two_market_db();
    let before = untouched_counts(&db);
    assert_eq!(before, (1, 1, 1));
    let b_before = market_rows(&db, "aave_v3_users", 2);

    {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        DegenbotDb::purge_aave_market_on_conn(&tx, 1).unwrap();
        tx.commit().unwrap();
    }

    // Market A is drained in every market-scoped relation.
    assert_eq!(market_rows(&db, "aave_v3_users", 1), 0);
    assert_eq!(market_rows(&db, "aave_v3_assets", 1), 0);
    assert_eq!(market_rows(&db, "aave_v3_emode_categories", 1), 0);
    let a_collateral: i64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM aave_v3_collateral_positions p \
             JOIN aave_v3_users u ON u.id = p.user_id WHERE u.market_id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(a_collateral, 0);
    let b_collateral: i64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM aave_v3_collateral_positions p \
             JOIN aave_v3_users u ON u.id = p.user_id WHERE u.market_id = 2",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(b_collateral, 1, "market B's positions survive");
    // The address-provider row that anchors the cold-boot bootstrap survives.
    let ap: i64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM aave_v3_contracts \
             WHERE market_id = 1 AND name = 'POOL_ADDRESS_PROVIDER'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(ap, 1);
    // The two children of `aave_v3_assets` for market A are gone with it.
    let a_asset_configs: i64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM aave_v3_asset_configs c \
             JOIN aave_v3_assets a ON a.id = c.asset_id WHERE a.market_id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(a_asset_configs, 0);
    // Market B is byte-for-byte intact.
    assert_eq!(market_rows(&db, "aave_v3_users", 2), b_before);
    assert_eq!(market_rows(&db, "aave_v3_assets", 2), 1);
    assert_eq!(market_rows(&db, "aave_v3_contracts", 2), 2);
    let b_child_rows: i64 = {
        let conn = db.lock();
        conn.query_row(
            "SELECT (SELECT COUNT(*) FROM aave_v3_asset_configs c \
                     JOIN aave_v3_assets a ON a.id = c.asset_id WHERE a.market_id = 2) \
                  + (SELECT COUNT(*) FROM aave_v3_user_collateral_configs uc \
                     JOIN aave_v3_assets a ON a.id = uc.asset_id WHERE a.market_id = 2)",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(b_child_rows, 2, "market B's asset-config children survive");
    // The non-market tables are untouched.
    assert_eq!(untouched_counts(&db), before);
}

#[test]
fn reset_rewinds_the_cursor_and_purge_only_removes_its_own_transaction() {
    let db = two_market_db();
    {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        DegenbotDb::reset_aave_market_on_conn(&tx, 1, 16_291_070).unwrap();
        tx.commit().unwrap();
    }
    let conn = db.lock();
    let stamp: Option<i64> = conn
        .query_row(
            "SELECT last_update_block FROM aave_v3_markets WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stamp, Some(16_291_070));
    let other: Option<i64> = conn
        .query_row(
            "SELECT last_update_block FROM aave_v3_markets WHERE id = 2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(other, Some(500), "market B's cursor is not rewound");
}

#[test]
fn a_dropped_transaction_leaves_a_purge_uncommitted() {
    let db = two_market_db();
    let users_before = market_rows(&db, "aave_v3_users", 1);
    {
        let mut guard = db.lock();
        let tx = guard.transaction().unwrap();
        DegenbotDb::purge_aave_market_on_conn(&tx, 1).unwrap();
        // Drop without committing: the purge rolls back whole.
        drop(tx);
    }
    assert_eq!(market_rows(&db, "aave_v3_users", 1), users_before);
    assert_eq!(market_rows(&db, "aave_v3_assets", 1), 1);
}

#[test]
fn purge_of_an_unknown_market_reports_the_missing_row() {
    let db = two_market_db();
    let mut guard = db.lock();
    let tx = guard.transaction().unwrap();
    let err = DegenbotDb::purge_aave_market_on_conn(&tx, 999).unwrap_err();
    assert!(
        matches!(err, DbError::MissingRow(_)),
        "expected MissingRow, got {err:?}"
    );
}
