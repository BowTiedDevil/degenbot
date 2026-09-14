//! §4.2 parity for the READ-ONLY candidate-pool discovery surface
//! (`degenbot-db::discovery_read`, ergo YFIOSF / Gap G2).
//!
//! Opens the frozen Alembic-stamped `fixtures/parity.db` (chain 8453: one
//! `uniswap_v3` pool registered under the `aerodrome_v3` exchange + one
//! Uniswap V4 managed pool under the `uniswap_v4` manager) and asserts the
//! discovery rows carry every column the Python `build_paths.py` ->
//! `PoolBuilder` construction path reads: the base `pools` fields, the
//! token0/token1 `erc20_tokens` join (address + decimals), the owning
//! `exchanges` row (factory + `last_update_block`), and the per-family
//! fee / tick-spacing / Aerodrome `stable` / V4 `PoolManager` detail.
//!
//! The held-deferred-tx variant is exercised through `SnapshotDb` — the
//! discipline `build_paths.py` relies on for a frozen startup DB cut.

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // parity/integration test harness
use std::path::PathBuf;

use degenbot_db::snapshot_db::SnapshotDb;
use degenbot_db::{DegenbotDb, DiscoveryPoolRow, SchemaState};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
const CHAIN_ID: i64 = 8453;

fn fixture_db_path() -> PathBuf {
    PathBuf::from(FIXTURE_DIR).join("parity.db")
}

fn open_db() -> DegenbotDb {
    let (db, state) = DegenbotDb::open(&fixture_db_path())
        .unwrap_or_else(|e| panic!("open {}: {e}", fixture_db_path().display()));
    assert_eq!(state, SchemaState::AlembicCurrent);
    db
}

#[test]
fn fetch_discovery_rows_yields_v3_and_v4_for_chain() {
    let db = open_db();
    let rows = db.fetch_discovery_rows(CHAIN_ID).unwrap();
    assert_eq!(rows.len(), 2, "fixture has one V3 + one V4 pool");
    assert!(
        matches!(rows[0], DiscoveryPoolRow::V3(_)),
        "first row is the uniswap_v3 pool"
    );
    assert!(
        matches!(rows[1], DiscoveryPoolRow::V4(_)),
        "second row is the uniswap_v4 managed pool"
    );
}

#[test]
fn discovery_row_v3_carries_every_build_paths_column() {
    let db = open_db();
    let rows = db.fetch_discovery_rows(CHAIN_ID).unwrap();
    let DiscoveryPoolRow::V3(v3) = &rows[0] else {
        panic!("expected V3 row, got {:?}", rows[0]);
    };

    // base `pools` row
    assert_eq!(v3.pool.id, 1);
    assert_eq!(v3.pool.chain, CHAIN_ID);
    assert_eq!(v3.pool.kind, "uniswap_v3");
    assert_eq!(
        v3.pool.address.to_checksum(None),
        "0x7b8c1d2E3f4a5b6c7d8E9f0A1b2C3D4E5f6a7b8c"
    );
    // token0/token1 join (address + decimals)
    assert_eq!(v3.token0.symbol.as_deref(), Some("TKA"));
    assert_eq!(v3.token0.decimals, Some(18));
    assert_eq!(v3.token1.symbol.as_deref(), Some("TKB"));
    assert_eq!(v3.token1.decimals, Some(18));
    assert_eq!(v3.pool.token0_id, v3.token0.id);
    assert_eq!(v3.pool.token1_id, v3.token1.id);
    // exchanges row — the V3 pool's exchange is `aerodrome_v3`
    assert_eq!(v3.exchange.name, "aerodrome_v3");
    assert_eq!(v3.exchange.chain_id, CHAIN_ID);
    assert!(v3.exchange.active);
    assert_eq!(v3.exchange.last_update_block, Some(12_345_000));
    assert_eq!(
        v3.exchange.factory.to_checksum(None),
        "0x1AE92F98d07afFD821725Cd463C223c1E8C5A6d2"
    );
    assert_eq!(v3.pool.exchange_id, v3.exchange.id);
    // V3 subclass detail — fees, tick spacing, liquidity marker
    assert_eq!(
        (v3.fee_token0, v3.fee_token1, v3.fee_denominator),
        (3, 3, 1000)
    );
    assert_eq!(v3.tick_spacing, 60);
    assert_eq!(v3.liquidity_update_block, Some(12_345_000));
    assert_eq!(v3.liquidity_update_log_index, Some(42));
}

#[test]
fn discovery_row_v4_carries_every_build_paths_column() {
    let db = open_db();
    let rows = db.fetch_discovery_rows(CHAIN_ID).unwrap();
    let DiscoveryPoolRow::V4(v4) = &rows[1] else {
        panic!("expected V4 row, got {:?}", rows[1]);
    };

    // managed_pools + uniswap_v4_pools detail
    assert_eq!(v4.managed_pool_id, 1);
    assert_eq!(
        format!("{:#x}", v4.pool_hash),
        "0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a"
    );
    assert_eq!(
        v4.hooks.to_checksum(None),
        "0x0000000000000000000000000000000000000000"
    );
    assert_eq!(
        (v4.fee_currency0, v4.fee_currency1, v4.fee_denominator),
        (0, 0, 1_000_000)
    );
    assert_eq!(v4.tick_spacing, 60);
    assert_eq!(v4.liquidity_update_block, Some(12_340_000));
    assert_eq!(v4.liquidity_update_log_index, Some(7));
    // currency0/currency1 token join
    assert_eq!(v4.token0.id, 1);
    assert_eq!(v4.token0.decimals, Some(18));
    assert_eq!(v4.token1.id, 3);
    assert_eq!(v4.token1.decimals, Some(6));
    assert_eq!(v4.token1.symbol.as_deref(), Some("USDC"));
    // pool_managers row (address + state_view) and its exchange
    assert_eq!(
        v4.manager.address.to_checksum(None),
        "0x498581fF718922c3f8e6A244956aF099B2652b2b"
    );
    assert_eq!(v4.manager.chain, CHAIN_ID);
    assert_eq!(
        v4.manager.state_view.unwrap().to_checksum(None),
        "0x0000000000000000000000000000000000000001"
    );
    assert_eq!(v4.exchange.name, "uniswap_v4");
    assert_eq!(
        v4.exchange.factory.to_checksum(None),
        "0x000000000004444c5DC75Cb358380D8E63429569"
    );
    assert_eq!(v4.manager.exchange_id, v4.exchange.id);
}

#[test]
fn discovery_rows_for_unknown_chain_are_empty() {
    let db = open_db();
    let rows = db.fetch_discovery_rows(999_999).unwrap();
    assert!(rows.is_empty(), "unknown chain must discover nothing");
}

#[test]
fn snapshot_db_discovery_uses_held_tx_and_matches_direct_read() {
    let direct = open_db().fetch_discovery_rows(CHAIN_ID).unwrap();
    let (snap, state) = SnapshotDb::open(&fixture_db_path()).expect("open SnapshotDb");
    assert_eq!(state, SchemaState::AlembicCurrent);
    let from_snapshot = snap
        .fetch_discovery_rows(CHAIN_ID)
        .expect("held-tx discovery read");
    assert_eq!(
        from_snapshot, direct,
        "SnapshotDb discovery must match the direct DegenbotDb read"
    );
    snap.commit().unwrap();
}
