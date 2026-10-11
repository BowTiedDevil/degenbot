//! The word-bitmap VALUE-table retirement migration.
//!
//! An OLD database written at the previous Rust schema version (2) still
//! carries `initialization_maps` / `managed_pool_initialization_maps`, both
//! POPULATED. Opening it through the new binary must run the forward
//! migration (step 3 drops the retired tables) and land `RustOwned` at the
//! current version WITHOUT a panic/unwrap/expect firing merely because the
//! old tables exist, and WITHOUT losing data a live path still reads
//! (`liquidity_positions`).
//!
//! Two cases:
//!
//! 1. `old_db_with_populated_word_tables_migrates_cleanly` — the tables are
//!    present and populated; the migration drops them, the live table
//!    survives.
//! 2. `old_db_without_word_tables_migrates_cleanly` — the tolerance probe:
//!    the migration step is IDEMPOTENT (`DROP TABLE IF EXISTS`), so a DB
//!    already stripped of the tables still migrates. Reverting that tolerance
//!    (plain `DROP TABLE`) makes THIS case fail loudly with
//!    `DbError::MigrationStepFailed` — the recorded negative probe.

#![expect(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;

use degenbot_db::schema::{RUST_SCHEMA_VERSION, SCHEMA_VERSION_TABLE};
use degenbot_db::{DegenbotDb, SchemaState};

/// The retired word-value tables.
const RETIRED: &[&str] = &["initialization_maps", "managed_pool_initialization_maps"];

fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        rusqlite::params![name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

fn stamp(conn: &Connection) -> i64 {
    conn.query_row(
        &format!("SELECT schema_version FROM {SCHEMA_VERSION_TABLE}"),
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// Seed an old-version (2) Rust-owned file: the head schema as the PREVIOUS
/// binary left it (with the two word tables when `word_tables`), populated
/// rows, a `liquidity_positions` row (the live data that must survive), and
/// the schema stamp rolled back to 2.
fn build_old_db(path: &Path, word_tables: bool) {
    // 1. Create a current-schema file (the retired tables are no longer in the
    //    head DDL, so the old DB is reconstructed explicitly below).
    let (db, state) = DegenbotDb::open(path).expect("fresh open");
    assert!(matches!(state, SchemaState::FreshStandalone { .. }));
    drop(db);

    // 2. Roll it back to the PREVIOUS schema shape + stamp.
    let conn = Connection::open(path).unwrap();
    // Parent rows the FK-bearing inserts below reference: a self-consistent
    // synthetic old DB (erc20 -> exchanges -> pools / pool_managers ->
    // managed_pools) mirroring what the previous binary would have written.
    conn.execute_batch(
        "INSERT INTO erc20_tokens (id, chain, address) VALUES \
             (1, 1, '0x0000000000000000000000000000000000000001'), \
             (2, 1, '0x0000000000000000000000000000000000000002');\
         INSERT INTO exchanges (id, chain_id, name, active, factory) VALUES \
             (1, 1, 'uniswap_v3', 0, '0x00000000000000000000000000000000000000f0');\
         INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) \
             VALUES (1, '0x00000000000000000000000000000000000000a1', 1, 'uniswap_v3', 1, 2, 1);\
         INSERT INTO pool_managers (id, address, chain, kind, exchange_id) VALUES \
             (1, '0x00000000000000000000000000000000000000b1', 1, 'uniswap_v4', 1);\
         INSERT INTO managed_pools (id, kind, manager_id) VALUES (1, 'uniswap_v4', 1);",
    )
    .unwrap();
    if word_tables {
        conn.execute_batch(
            "CREATE TABLE initialization_maps (\
                 id INTEGER NOT NULL, pool_id INTEGER NOT NULL, word INTEGER NOT NULL, \
                 bitmap VARCHAR(78) NOT NULL, PRIMARY KEY (id));\
             CREATE TABLE managed_pool_initialization_maps (\
                 id INTEGER NOT NULL, managed_pool_id INTEGER NOT NULL, \
                 word INTEGER NOT NULL, bitmap VARCHAR(78) NOT NULL, PRIMARY KEY (id));",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO initialization_maps (pool_id, word, bitmap) VALUES (1, 0, '3')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO managed_pool_initialization_maps (managed_pool_id, word, bitmap) \
             VALUES (1, 0, '3')",
            [],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO liquidity_positions (pool_id, tick, liquidity_net, liquidity_gross) \
         VALUES (1, 10, '5', '5')",
        [],
    )
    .unwrap();
    conn.execute_batch(&format!(
        "DELETE FROM {SCHEMA_VERSION_TABLE};\
         INSERT INTO {SCHEMA_VERSION_TABLE} (schema_version) VALUES (2);"
    ))
    .unwrap();
    drop(conn);
}

fn run_case(word_tables: bool) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("old.db");
    build_old_db(&path, word_tables);

    // Sanity: the fixture really is at the previous version.
    {
        let conn = Connection::open(&path).unwrap();
        assert_eq!(stamp(&conn), 2, "fixture must start at schema version 2");
        for t in RETIRED {
            assert_eq!(
                has_table(&conn, t),
                word_tables,
                "fixture word-table presence for {t}"
            );
        }
    }

    // The open under test: no panic/unwrap/expect may fire merely because the
    // old tables exist.
    let (db, state) = DegenbotDb::open(&path).expect("old DB opens through migration");
    assert_eq!(
        state,
        SchemaState::RustOwned {
            schema_version: RUST_SCHEMA_VERSION,
        },
        "migration advances the stamp to the current version",
    );

    let conn = db.lock();
    assert_eq!(stamp(&conn), i64::from(RUST_SCHEMA_VERSION));
    for t in RETIRED {
        assert!(
            !has_table(&conn, t),
            "the migration must drop the retired table {t}",
        );
    }
    // Data a live path still reads survived the migration.
    let live: i64 = conn
        .query_row("SELECT COUNT(*) FROM liquidity_positions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(live, 1, "live liquidity_positions rows must survive");
}

#[test]
fn old_db_with_populated_word_tables_migrates_cleanly() {
    run_case(true);
}

#[test]
fn old_db_without_word_tables_migrates_cleanly() {
    run_case(false);
}
