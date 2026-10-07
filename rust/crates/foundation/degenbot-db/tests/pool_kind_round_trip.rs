//! The persisted `kind` strings are on-disk values: this round-trip writes one
//! pool per taxonomy member (the supported roster + the declared-unsupported
//! LFJ pair) into a freshly bootstrapped database, reads every row back
//! through the canonical vocabulary, and asserts the string that comes off
//! disk is byte-identical to the string that went in and still maps to the
//! same graph family. A taxonomy rename cannot pass this file.
//!
//! The write half uses raw SQL because the reader handle is query-only by
//! design; the row shapes mirror the upstream writer's (`erc20_tokens` →
//! `exchanges` → `pools` → subclass, and the V4 `pool_managers` →
//! `managed_pools` → `uniswap_v4_pools` chain whose `managed_pools.kind`
//! carries the discriminator).

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture test: every unwrap/panic is a hard precondition of the round-trip"
)]

use degenbot_db::schema::table::{is_lfj_kind, v2_v3_subclass_table, LFJ_POOLS};
use degenbot_db::{DegenbotDb, PoolKind, PoolKindRow, SchemaState};
use rusqlite::Connection;
use tempfile::TempDir;

/// One entry per taxonomy member: the persisted `kind` string and the graph
/// family it must keep mapping to after the round-trip (`None` = the
/// declared-unsupported LFJ pair, which classifies but admits no family).
const TAXONOMY: &[(&str, Option<PoolKind>)] = &[
    ("uniswap_v2", Some(PoolKind::V2)),
    ("sushiswap_v2", Some(PoolKind::V2)),
    ("pancakeswap_v2", Some(PoolKind::V2)),
    ("aerodrome_v2", Some(PoolKind::V2)),
    ("camelot_v2", Some(PoolKind::V2)),
    ("swapbased_v2", Some(PoolKind::V2)),
    ("uniswap_v3", Some(PoolKind::V3)),
    ("sushiswap_v3", Some(PoolKind::V3)),
    ("pancakeswap_v3", Some(PoolKind::V3)),
    ("aerodrome_v3", Some(PoolKind::V3)),
    ("uniswap_v4", Some(PoolKind::V4)),
    ("lfj_binned", None),
];

/// The table whose `kind` column carries the discriminator for one member:
/// the polymorphic `managed_pools` base for V4, the `pools` base otherwise.
fn kind_table(kind: &str) -> &'static str {
    if kind == "uniswap_v4" {
        "managed_pools"
    } else {
        "pools"
    }
}

/// Insert the FK-consistent rows one taxonomy member needs.
fn insert_pool(conn: &Connection, pool_id: i64, kind: &str) {
    if kind == "uniswap_v4" {
        conn.execute(
            "INSERT INTO pool_managers (id, address, chain, kind, state_view, exchange_id) \
             VALUES (?1, '0x0000000000000000000000000000000000000004', 1, 'uniswap_v4', NULL, 1)",
            rusqlite::params![pool_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO managed_pools (id, kind, manager_id) VALUES (?1, ?2, ?1)",
            rusqlite::params![pool_id, kind],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO uniswap_v4_pools (managed_pool_id, pool_hash, hooks, currency0_id, \
             currency1_id, fee_currency0, fee_currency1, fee_denominator, tick_spacing, \
             liquidity_update_block, liquidity_update_log_index) \
             VALUES (?1, ?2, '0x0000000000000000000000000000000000000005', 1, 2, 3000, 3000, \
             1000000, 60, NULL, NULL)",
            rusqlite::params![pool_id, format!("0x{pool_id:064x}")],
        )
        .unwrap();
        return;
    }
    conn.execute(
        "INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) \
         VALUES (?1, ?2, 1, ?3, 1, 2, 1)",
        rusqlite::params![pool_id, format!("0x{pool_id:040x}"), kind],
    )
    .unwrap();
    if is_lfj_kind(kind) {
        // The declared-unsupported binned pair: typed in the row vocabulary,
        // admitted by no tier.
        assert_eq!(kind, "lfj_binned");
        conn.execute(
            &format!("INSERT INTO {LFJ_POOLS} (pool_id, bin_step) VALUES (?1, 20)"),
            rusqlite::params![pool_id],
        )
        .unwrap();
    } else if let Some(sub) = v2_v3_subclass_table(kind) {
        if sub == "aerodrome_v2_pools" {
            // The only V2 subclass with a `stable` column.
            conn.execute(
                "INSERT INTO aerodrome_v2_pools (pool_id, fee_token0, fee_token1, \
                 fee_denominator, stable) VALUES (?1, 3000, 3000, 10000, 0)",
                rusqlite::params![pool_id],
            )
            .unwrap();
        } else if sub.ends_with("_v2_pools") {
            conn.execute(
                &format!(
                    "INSERT INTO {sub} (pool_id, fee_token0, fee_token1, fee_denominator) \
                     VALUES (?1, 3000, 3000, 10000)"
                ),
                rusqlite::params![pool_id],
            )
            .unwrap();
        } else {
            conn.execute(
                &format!(
                    "INSERT INTO {sub} (pool_id, tick_spacing, liquidity_update_block, \
                     liquidity_update_log_index, fee_token0, fee_token1, fee_denominator) \
                     VALUES (?1, 60, NULL, NULL, 3000, 3000, 10000)"
                ),
                rusqlite::params![pool_id],
            )
            .unwrap();
        }
    } else {
        panic!("unroutable fixture kind {kind:?} (taxonomy drift?)");
    }
}

#[test]
fn persisted_kind_strings_round_trip_through_the_vocabulary() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("degenbot.db");

    // 1. Bootstrap the head schema through the public open seam.
    {
        let (_db, state) = DegenbotDb::open(&db_path).unwrap();
        // A fresh standalone file reports the one-shot creation disposition.
        assert!(matches!(state, SchemaState::FreshStandalone { .. }));
    }

    // 2. Write one pool per taxonomy member (the reader handle is
    //    query-only; the raw connection stands in for the upstream writer).
    {
        let conn = Connection::open(&db_path).unwrap();
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
        conn.execute(
            "INSERT INTO exchanges (id, chain_id, name, active, last_update_block, factory, \
             deployer) VALUES (1, 1, 'uniswap_v2', 1, NULL, \
             '0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f', NULL)",
            [],
        )
        .unwrap();
        for (index, (kind, _)) in TAXONOMY.iter().enumerate() {
            insert_pool(&conn, i64::try_from(index).unwrap() + 1, kind);
        }
    }

    // 3. Read back through the canonical vocabulary and prove the on-disk
    //    strings survived it untouched.
    let (db, state) = DegenbotDb::open(&db_path).unwrap();
    assert!(
        matches!(state, SchemaState::RustOwned { .. }),
        "second open is a no-op"
    );
    let read = Connection::open(&db_path).unwrap();
    for (index, (kind, expected_family)) in TAXONOMY.iter().enumerate() {
        let pool_id = i64::try_from(index).unwrap() + 1;
        let table = kind_table(kind);
        let on_disk: String = read
            .query_row(
                &format!("SELECT kind FROM {table} WHERE id = ?1"),
                rusqlite::params![pool_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(on_disk, *kind, "the persisted kind string was renamed");

        if let Some(family) = expected_family {
            assert_eq!(
                PoolKind::try_from(on_disk.as_str()),
                Ok(*family),
                "{kind} no longer maps to its graph family"
            );
        } else {
            // Declared-unsupported, not unknown: classifiable, refused by
            // the graph vocabulary.
            assert!(PoolKind::try_from(on_disk.as_str()).is_err());
            assert!(PoolKind::is_declared_unsupported(on_disk.as_str()));
        }

        let row = db
            .fetch_pool_kind(kind, pool_id)
            .unwrap()
            .unwrap_or_else(|| panic!("{kind} row did not read back"));
        assert_eq!(
            row.graph_kind(),
            *expected_family,
            "{kind} row projects to the wrong family"
        );
        match (&row, expected_family) {
            (PoolKindRow::V2(..), Some(PoolKind::V2))
            | (PoolKindRow::V3(..), Some(PoolKind::V3))
            | (PoolKindRow::V4(..), Some(PoolKind::V4))
            | (PoolKindRow::Lfj(..), None) => {}
            (row, family) => panic!("row/family mismatch: {row:?} vs {family:?}"),
        }
    }
}
