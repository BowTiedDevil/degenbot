//! Tests for the out-of-place heal (ADR-011). See `heal.rs` for the contract.

use rusqlite::Connection;

use super::*;
use crate::migrate::SchemaState;
use crate::ops::create_new_database;
use crate::schema::RUST_SCHEMA_VERSION;

// ── helpers ──────────────────────────────────────────────────────────────

/// Count rows in `table` on `conn`.
fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |r| {
        r.get(0)
    })
    .unwrap()
}

/// `true` if a table named `name` exists in `conn`'s `sqlite_master`.
fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        rusqlite::params![name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

/// Flip a Rust-owned DB to the legacy `alembic_version`-marked shape so
/// `heal_database` has a legacy source to rebuild.
fn mark_legacy(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "DROP TABLE _degenbot_db_schema_version;\n\
         CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
         INSERT INTO alembic_version (version_num) VALUES ('e0aaad8ad486');",
    )
    .unwrap();
}

/// Populate a head-schema DB with a small, FK-consistent dataset spanning the
/// core parent→child graph: `erc20_tokens` → `exchanges` → `pools` → `v2` subclass +
/// `liquidity_positions`.
fn populate_head_dataset(conn: &Connection) {
    // erc20_tokens (parent of pools). The UNIQUE index is (address, chain).
    for (id, addr) in [
        (1, "0x0000000000000000000000000000000000000001"),
        (2, "0x0000000000000000000000000000000000000002"),
    ] {
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address, name, symbol, decimals) \
             VALUES (?1, 1, ?2, 'T', 'T', 18)",
            rusqlite::params![id, addr],
        )
        .unwrap();
    }
    // exchanges (factory + deployer; both head-schema columns).
    conn.execute(
        "INSERT INTO exchanges (id, chain_id, name, active, last_update_block, factory, deployer) \
         VALUES (1, 1, 'uniswap_v2', 1, NULL, '0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f', NULL)",
        [],
    )
    .unwrap();
    // pools (FK token0_id/token1_id → erc20_tokens, exchange_id → exchanges).
    conn.execute(
        "INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) \
         VALUES (1, '0x000000000000000000000000000000000000000a', 1, 'uniswap_v2', 1, 2, 1)",
        [],
    )
    .unwrap();
    // uniswap_v2_pools subclass (FK pool_id → pools; PK pool_id, no AUTOINCREMENT).
    conn.execute(
        "INSERT INTO uniswap_v2_pools (pool_id, fee_token0, fee_token1, fee_denominator) \
         VALUES (1, 3000, 3000, 10000)",
        [],
    )
    .unwrap();
    // liquidity_positions (FK pool_id → pools).
    conn.execute(
        "INSERT INTO liquidity_positions (id, pool_id, tick, liquidity_net, liquidity_gross) \
         VALUES (1, 1, -100, '1000', '2000')",
        [],
    )
    .unwrap();
}

/// Walk every `FOREIGN KEY` declaration on every content table and confirm each
/// referenced row exists in the parent table.
fn assert_fk_intact(conn: &Connection) {
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='table' \
             AND name NOT LIKE 'sqlite_%' AND name NOT IN ('alembic_version','_degenbot_db_schema_version')",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for table in &tables {
        let fks: Vec<(String, String)> = conn
            .prepare(&format!("PRAGMA foreign_key_list(\"{table}\")"))
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(2)?, r.get::<_, String>(3)?)))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        for (parent, from_col) in fks {
            if !has_table(conn, &parent) {
                continue;
            }
            let missing: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM \"{table}\" WHERE \"{from_col}\" IS NOT NULL \
                         AND \"{from_col}\" NOT IN (SELECT id FROM \"{parent}\")"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(missing, 0, "FK break: {table}.{from_col} → {parent}");
        }
    }
}

/// Every content table's row count must equal `expected`.
fn assert_row_counts(conn: &Connection, expected: &[(&str, i64)]) {
    for (table, n) in expected {
        assert_eq!(count(conn, table), *n, "row count for {table}");
    }
}

// ── 1. legacy → heal ─────────────────────────────────────────────────────

#[test]
fn legacy_to_heal() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("legacy.db");
    create_new_database(&db_path).unwrap();
    {
        let conn = Connection::open(&db_path).unwrap();
        populate_head_dataset(&conn);
    }
    mark_legacy(&db_path);
    let expected_counts = [
        ("erc20_tokens", 2),
        ("exchanges", 1),
        ("pools", 1),
        ("uniswap_v2_pools", 1),
        ("liquidity_positions", 1),
    ];

    let report = heal_database(&db_path).unwrap();

    // Outcome: RustOwned, alembic_version GONE, Rust stamp table present.
    assert_eq!(
        report.new_state,
        SchemaState::RustOwned {
            schema_version: RUST_SCHEMA_VERSION,
        }
    );
    assert!(matches!(report.old_state, SchemaState::LegacyAlembic));
    let probe = Connection::open(&db_path).unwrap();
    assert!(!has_table(&probe, "alembic_version"));
    assert!(has_table(&probe, "_degenbot_db_schema_version"));

    // Per-table row counts copied (and preserved post-swap).
    assert_row_counts(&probe, &expected_counts);
    for (t, n) in expected_counts {
        assert_eq!(
            report.rows_copied.get(t).copied(),
            Some(u64::try_from(n).unwrap()),
            "rows_copied for {t}"
        );
    }

    // FK integrity intact: every _id resolves to an existing parent row.
    assert_fk_intact(&probe);

    // The .bak exists, is readable, and holds the OLD (pre-heal) data.
    assert!(report.bak_path.exists());
    let bak = Connection::open(&report.bak_path).unwrap();
    assert_row_counts(&bak, &expected_counts);
    // The old DB carried the legacy marker.
    assert!(has_table(&bak, "alembic_version"));
}

// ── 2. divergent legacy schema → heal (index restored) ────────────────────

#[test]
fn divergent_legacy_to_heal() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("stale.db");
    create_new_database(&db_path).unwrap();
    {
        let conn = Connection::open(&db_path).unwrap();
        // Drop a head index to make the old schema divergent from head.
        conn.execute("DROP INDEX ix_erc20_tokens_chain", [])
            .unwrap();
        populate_head_dataset(&conn);
    }
    mark_legacy(&db_path);

    // Pre-condition: the fixture is legacy and the index is absent.
    assert_eq!(
        crate::ops::inspect_schema_state(&db_path).unwrap(),
        SchemaState::LegacyAlembic
    );
    let stale_probe = Connection::open(&db_path).unwrap();
    let ix_before: i64 = stale_probe
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='ix_erc20_tokens_chain'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ix_before, 0);

    let report = heal_database(&db_path).unwrap();
    assert!(matches!(report.old_state, SchemaState::LegacyAlembic));
    assert_eq!(
        report.new_state,
        SchemaState::RustOwned {
            schema_version: RUST_SCHEMA_VERSION,
        }
    );

    let probe = Connection::open(&db_path).unwrap();
    // create_new_database re-applied the head DDL → the index is back.
    let ix_after: i64 = probe
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='ix_erc20_tokens_chain'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ix_after, 1);
    assert!(!has_table(&probe, "alembic_version"));

    assert_row_counts(
        &probe,
        &[
            ("erc20_tokens", 2),
            ("exchanges", 1),
            ("pools", 1),
            ("uniswap_v2_pools", 1),
            ("liquidity_positions", 1),
        ],
    );
    assert_fk_intact(&probe);
}

// ── 3. unrecognized refusal ───────────────────────────────────────────────

#[test]
fn unrecognized_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("foreign.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE other (x INTEGER);")
            .unwrap();
    }

    let err = heal_database(&db_path).unwrap_err();
    assert!(matches!(err, DbError::UnrecognizedSchema));

    // No swap, no .bak, live DB untouched.
    assert!(db_path.exists());
    let probe = Connection::open(&db_path).unwrap();
    assert!(has_table(&probe, "other"));
    assert!(!report_bak_exists(&db_path));
    assert!(!dir.path().join("foreign.db.heal-tmp").exists());
}

// ── 4. already RustOwned → no-op ──────────────────────────────────────────

#[test]
fn already_rustowned_noop() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("rustowned.db");
    create_new_database(&db_path).unwrap();
    assert_eq!(
        crate::ops::inspect_schema_state(&db_path).unwrap(),
        SchemaState::RustOwned {
            schema_version: RUST_SCHEMA_VERSION,
        }
    );
    // Snapshot the file's mtime to prove it wasn't swapped.
    let mtime_before = std::fs::metadata(&db_path).unwrap().modified().unwrap();

    let report = heal_database(&db_path).unwrap();
    assert!(matches!(report.old_state, SchemaState::RustOwned { .. }));
    assert!(report.rows_copied.is_empty());
    // bak_path == old_path (no backup taken for the no-op).
    assert_eq!(report.bak_path, db_path);
    assert!(!report_bak_exists(&db_path));

    let mtime_after = std::fs::metadata(&db_path).unwrap().modified().unwrap();
    assert_eq!(mtime_before, mtime_after, "RustOwned heal must not swap");
}

// ── 5. verification failure + cleanup ─────────────────────────────────────

#[test]
fn verify_row_counts_detects_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.db");
    let tmp = dir.path().join("tmp.db");
    create_new_database(&old).unwrap();
    create_new_database(&tmp).unwrap();
    {
        let c = Connection::open(&old).unwrap();
        c.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, 1, '0xabc')",
            [],
        )
        .unwrap();
    }
    // tmp has 0 erc20_tokens rows; old has 1 → mismatch.
    let err = super::verify_row_counts(&old, &tmp).unwrap_err();
    assert!(matches!(
        err,
        DbError::HealVerificationFailed {
            table,
            old_count: 1,
            new_count: 0,
        } if table == "erc20_tokens"
    ));
}

#[test]
fn heal_failure_leaves_live_db_untouched_and_cleans_temp() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("dup.db");
    create_new_database(&db_path).unwrap();
    {
        let c = Connection::open(&db_path).unwrap();
        // First token row (legit).
        c.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, 1, '0xabc')",
            [],
        )
        .unwrap();
        // Drop the UNIQUE (address, chain) index on the OLD DB so a duplicate
        // can sneak in — the NEW (head-schema) DB still enforces it.
        c.execute("DROP INDEX ix_erc20_tokens_address_chain", [])
            .unwrap();
        c.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (2, 1, '0xabc')",
            [],
        )
        .unwrap();
    }
    // Mark legacy so the heal has a legacy source.
    mark_legacy(&db_path);
    // Pre-heal: old has 2 rows.
    assert_eq!(
        count(&Connection::open(&db_path).unwrap(), "erc20_tokens"),
        2
    );

    let err = heal_database(&db_path).unwrap_err();
    // The copy fails mid-table (UNIQUE violation) → `copy_table` propagates the
    // `rusqlite` error immediately as `DbError::Sqlite`.
    assert!(
        matches!(err, DbError::Sqlite(_)),
        "expected Sqlite (copy failure), got {err:?}"
    );

    // Live DB untouched: both rows still present, no .bak, no temp.
    let probe = Connection::open(&db_path).unwrap();
    assert_eq!(count(&probe, "erc20_tokens"), 2);
    assert!(has_table(&probe, "alembic_version")); // still legacy-owned
    assert!(!report_bak_exists(&db_path));
    assert!(!dir.path().join("dup.db.heal-tmp").exists());
}

// ── 6. atomic swap keeps .bak with OLD data ───────────────────────────────

#[test]
fn atomic_swap_keeps_bak() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("swap.db");
    create_new_database(&db_path).unwrap();
    {
        let conn = Connection::open(&db_path).unwrap();
        populate_head_dataset(&conn);
    }
    mark_legacy(&db_path);

    let report = heal_database(&db_path).unwrap();
    assert_eq!(
        report.new_state,
        SchemaState::RustOwned {
            schema_version: RUST_SCHEMA_VERSION,
        }
    );

    // .bak exists + is readable read-only + holds the OLD data (pre-heal counts).
    assert!(report.bak_path.exists());
    let bak = Connection::open(&report.bak_path).unwrap();
    bak.execute_batch("PRAGMA query_only=on;").unwrap();
    assert_row_counts(
        &bak,
        &[
            ("erc20_tokens", 2),
            ("exchanges", 1),
            ("pools", 1),
            ("uniswap_v2_pools", 1),
            ("liquidity_positions", 1),
        ],
    );
    // The backup retains the OLD legacy ownership shape.
    assert!(has_table(&bak, "alembic_version"));
    assert!(!has_table(&bak, "_degenbot_db_schema_version"));

    // The live (healed) DB is RustOwned and lost alembic_version.
    let live = Connection::open(&db_path).unwrap();
    assert!(!has_table(&live, "alembic_version"));
    assert!(has_table(&live, "_degenbot_db_schema_version"));
}

// ── 7. old DB left byte-identical during copy (no -wal/-shm sidecars) ────

#[test]
fn heal_leaves_old_db_byte_identical_no_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("old.db");
    create_new_database(&db_path).unwrap();
    {
        let conn = Connection::open(&db_path).unwrap();
        populate_head_dataset(&conn);
    }
    mark_legacy(&db_path);
    // Snapshot the old file's bytes BEFORE heal — the .bak must equal this
    // exactly (proving the copy phase never wrote to or sidecar'd the old DB).
    let old_bytes = std::fs::read(&db_path).unwrap();

    let _ = heal_database(&db_path).unwrap();

    // No `-wal`/`-shm` sidecars were created on the old path.
    assert!(
        !db_path.with_extension("db-wal").exists(),
        "old -wal sidecar"
    );
    assert!(
        !db_path.with_extension("db-shm").exists(),
        "old -shm sidecar"
    );

    // The .bak is byte-for-byte the pre-heal old DB.
    let bak_path = db_path.with_file_name("old.db.bak");
    assert_eq!(std::fs::read(&bak_path).unwrap(), old_bytes);
}

fn report_bak_exists(db_path: &std::path::Path) -> bool {
    let mut s = db_path.file_name().unwrap().to_owned();
    s.push(".bak");
    db_path.with_file_name(s).exists()
}
