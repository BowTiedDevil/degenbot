//! Tests for the out-of-place heal (ADR-011). See `heal.rs` for the contract.

use std::collections::HashSet;

use rusqlite::Connection;

use super::*;
use crate::migrate::SchemaState;
use crate::ops::{convert_alembic_to_rust_owned, create_new_database};
use crate::schema::{ALEMBIC_HEAD, RUST_SCHEMA_VERSION};

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

/// Populate a head-schema DB with a small, FK-consistent dataset spanning the
/// core parent→child graph: `erc20_tokens` → `exchanges` → `pools` → `v2` subclass +
/// `liquidity_positions`. Returns the inserted counts per table.
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
/// referenced row exists in the parent table. This is the FK-integrity invariant
/// heal must preserve (PK `id` values are copied as-is so FK references survive).
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
        // PRAGMA foreign_key_list: cols (id, seq, table, from, to, on_update, on_delete, match).
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
            // Every non-NULL value in `from_col` must resolve to a parent row.
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

/// Every content table's row count must equal `expected` (best-effort: only the
/// tables we populated; others are 0).
fn assert_row_counts(conn: &Connection, expected: &[(&str, i64)]) {
    for (table, n) in expected {
        assert_eq!(count(conn, table), *n, "row count for {table}");
    }
}

/// Build the YWN7Z6 stale fixture in Rust: a head-schema DB stamped one revision
/// below `ALEMBIC_HEAD` (head minus the `ix_erc20_tokens_chain` index).
fn build_stale_fixture(path: &std::path::Path) {
    create_new_database(path).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute("DROP INDEX ix_erc20_tokens_chain", [])
        .unwrap();
    conn.execute("UPDATE alembic_version SET version_num='e0aaad8ad486'", [])
        .unwrap();
}

// ── 1. head → heal ───────────────────────────────────────────────────────

#[test]
fn head_to_heal() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("head.db");
    create_new_database(&db_path).unwrap();
    {
        let conn = Connection::open(&db_path).unwrap();
        populate_head_dataset(&conn);
    }
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
    assert!(matches!(report.old_state, SchemaState::AlembicCurrent));
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
    // The old DB was at head (alembic_version present in the backup).
    assert!(has_table(&bak, "alembic_version"));
}

// ── 2. stale → heal ─────────────────────────────────────────────────────

#[test]
fn stale_to_heal() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("stale.db");
    build_stale_fixture(&db_path);
    {
        let conn = Connection::open(&db_path).unwrap();
        populate_head_dataset(&conn);
    }

    // Pre-condition: the fixture is genuinely stale, index is absent.
    assert_eq!(
        crate::ops::inspect_schema_state(&db_path).unwrap(),
        SchemaState::AlembicStale {
            head: "e0aaad8ad486".to_string(),
            expected: ALEMBIC_HEAD.to_string(),
        }
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
    assert!(matches!(report.old_state, SchemaState::AlembicStale { .. }));
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

    // Rows preserved + FK intact.
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
    // No temp file left behind.
    assert!(!dir.path().join("foreign.db.heal-tmp").exists());
}

// ── 4. already RustOwned → no-op ──────────────────────────────────────────

#[test]
fn already_rustowned_noop() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("rustowned.db");
    create_new_database(&db_path).unwrap();
    convert_alembic_to_rust_owned(&db_path).unwrap(); // flip to RustOwned
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
//
// The full `heal_database` flow copies rows exactly (SELECT all → INSERT all).
// A genuine count mismatch where the copy *succeeds* but produces fewer rows
// than the old is therefore not honestly reachable through the normal flow: if
// an insert fails (e.g. a UNIQUE-constraint violation the new head-schema DB
// enforces but the old DB, with a dropped index, allows) `copy_table`
// propagates the `rusqlite` error immediately — `verify_row_counts` never runs.
//
// So we exercise both independently:
//
// (a) `verify_row_counts` directly with a mismatched pair →
//     `HealVerificationFailed` (the variant's contract). This is the brief's
//     sanctioned "test-only seam": test the private verify fn directly, since
//     the full flow can't honestly produce a non-failing-copy mismatch.
// (b) The full `heal_database` cleanup path on a real copy failure (UNIQUE
//     violation mid-copy → `Err(Sqlite)`). The key assertions are the cleanup
//     guarantees: live DB untouched, `.bak` absent, temp file gone.

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
    // Pre-heal: old has 2 rows.
    assert_eq!(
        count(&Connection::open(&db_path).unwrap(), "erc20_tokens"),
        2
    );

    let err = heal_database(&db_path).unwrap_err();
    // The copy fails mid-table (UNIQUE violation) → `copy_table` propagates the
    // `rusqlite` error immediately as `DbError::Sqlite` (the post-copy verify
    // step never runs, since the copy itself errored). The `HealVerificationFailed`
    // variant is exercised directly in `verify_row_counts_detects_mismatch`.
    assert!(
        matches!(err, DbError::Sqlite(_)),
        "expected Sqlite (copy failure), got {err:?}"
    );

    // Live DB untouched: both rows still present, no .bak, no temp.
    let probe = Connection::open(&db_path).unwrap();
    assert_eq!(count(&probe, "erc20_tokens"), 2);
    assert!(has_table(&probe, "alembic_version")); // still Alembic-owned
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
    // The backup retains the OLD Alembic ownership shape.
    assert!(has_table(&bak, "alembic_version"));
    assert!(!has_table(&bak, "_degenbot_db_schema_version"));

    // The live (healed) DB is RustOwned and lost alembic_version.
    let live = Connection::open(&db_path).unwrap();
    assert!(!has_table(&live, "alembic_version"));
    assert!(has_table(&live, "_degenbot_db_schema_version"));
}

// ── bonus: FK order is derived + parents precede children ────────────────

#[test]
fn fk_order_parents_before_children() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("fk.db");
    create_new_database(&db_path).unwrap();
    let conn = Connection::open(&db_path).unwrap();
    let order = super::fk_ordered_tables(&conn).unwrap();

    // Core parents must precede their children.
    let pos = |name: &str| order.iter().position(|t| t == name).unwrap();
    assert!(
        pos("erc20_tokens") < pos("pools"),
        "erc20_tokens before pools"
    );
    assert!(pos("exchanges") < pos("pools"), "exchanges before pools");
    assert!(
        pos("pools") < pos("uniswap_v2_pools"),
        "pools before v2 subclass"
    );
    assert!(pos("pools") < pos("liquidity_positions"));
    assert!(pos("managed_pools") < pos("uniswap_v4_pools"));
    assert!(pos("aave_v3_markets") < pos("aave_v3_users"));
    assert!(pos("aave_v3_assets") < pos("aave_v3_asset_configs"));

    // No table appears twice.
    let mut seen = HashSet::new();
    for t in &order {
        assert!(seen.insert(t.as_str()), "{t} duplicated in FK order");
    }
}

fn report_bak_exists(db_path: &std::path::Path) -> bool {
    let mut s = db_path.file_name().unwrap().to_owned();
    s.push(".bak");
    db_path.with_file_name(s).exists()
}
