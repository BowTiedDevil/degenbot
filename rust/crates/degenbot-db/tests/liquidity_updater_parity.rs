//! §4.2 parity for the V3/V4 DB-aware liquidity updater (task QJSCA5).
//!
//! Opens the frozen `fixtures/liquidity_updater_v{3,4}_initial.db` (built by
//! `fixtures/generate_liquidity_updater_parity.py` — REAL Alembic-stamped DBs
//! seeded with a pool row + a pre-existing `LiquidityMap`, including a STALE
//! tick that no event touches so the `delete_stale_*_positions` path is
//! exercised). Copies each to a tmp path (the apply mutates the DB), applies the
//! SAME event sequence as the Python blueprint through the Rust
//! [`DegenbotDb::apply_v3_liquidity_updates`] /
//! [`DegenbotDb::apply_v4_liquidity_updates`], + asserts the resulting rows +
//! `liquidity_update_block`/`log_index` marker match the JSON oracle dumped by
//! the Python path against the SAME fixture.
//!
//! To regenerate the fixtures:
//!   `uv run python rust/crates/degenbot-db/tests/fixtures/generate_liquidity_updater_parity.py`

use std::path::PathBuf;

use alloy::primitives::{I256, U256};
use degenbot_db::{DegenbotDb, SchemaState};
use serde::Deserialize;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

#[derive(Debug, Deserialize)]
struct Expected {
    chain_id: i64,
    #[allow(dead_code)]
    tick_spacing: i32,
    #[serde(default)]
    pool_address: String,
    #[serde(default)]
    pool_hash: String,
    #[serde(default)]
    pool_manager_chain: i64,
    /// `(tick, liquidity_net, liquidity_gross)` — sorted by tick.
    positions: Vec<PositionRow>,
    /// `(word, bitmap)` — sorted by word.
    initialization_maps: Vec<InitMapRow>,
    liquidity_update_block: Option<i64>,
    liquidity_update_log_index: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct PositionRow {
    tick: i32,
    liquidity_net: String,
    liquidity_gross: String,
}

#[derive(Debug, Deserialize)]
struct InitMapRow {
    word: i64,
    bitmap: String,
}

/// A `(block_number, log_index, tick_lower, tick_upper, liquidity_delta)` event
/// sequence — MUST match the Python generator's blueprint exactly (same blocks,
/// log indices, ticks, deltas) so the Rust apply converges to the same state.
fn v3_events() -> Vec<(u64, u64, i32, i32, I256)> {
    let pos = |n: i64| I256::try_from(n).unwrap();
    vec![
        // block 100, idx 0: Mint +500_000 to ticks -10..10.
        (100, 0, -10, 10, pos(500_000)),
        // block 100, idx 1: Mint +250_000 to a brand-new tick pair 100..110.
        (100, 1, 100, 110, pos(250_000)),
        // block 101, idx 0: Burn -200_000 from -10..10.
        (101, 0, -10, 10, pos(-200_000)),
        // block 102, idx 0: Burn that drives the -10..10 ticks to gross=0.
        (102, 0, -10, 10, pos(-1_300_000)),
    ]
}

fn v4_events() -> Vec<(u64, u64, i32, i32, I256)> {
    let pos = |n: i64| I256::try_from(n).unwrap();
    vec![
        // block 200, idx 0: ModifyLiquidity +500_000 to -10..10.
        (200, 0, -10, 10, pos(500_000)),
        // block 200, idx 1: ModifyLiquidity +250_000 to a brand-new pair 100..110.
        (200, 1, 100, 110, pos(250_000)),
        // block 201, idx 0: ModifyLiquidity -200_000 from -10..10.
        (201, 0, -10, 10, pos(-200_000)),
        // block 202, idx 0: ModifyLiquidity that drives the -10..10 ticks to gross=0.
        (202, 0, -10, 10, pos(-1_300_000)),
    ]
}

/// Copy the frozen initial-state fixture DB to a tmp path so the Rust apply
/// mutates a throwaway, not the committed file. Returns the tmp path.
fn copy_fixture_to_tmp(src: &str, suffix: &str) -> PathBuf {
    let src = PathBuf::from(FIXTURE_DIR).join(src);
    let tmp = std::env::temp_dir().join(format!(
        "degenbot_db_liquidity_updater_parity_{suffix}_{}.db",
        std::process::id()
    ));
    std::fs::copy(&src, &tmp).unwrap_or_else(|e| panic!("copy {}: {e}", src.display()));
    tmp
}

fn to_event(packed: (u64, u64, i32, i32, I256)) -> degenbot_db::LiquidityUpdateEvent {
    let (block_number, log_index, tick_lower, tick_upper, liquidity_delta) = packed;
    degenbot_db::LiquidityUpdateEvent {
        block_number,
        log_index,
        tick_lower,
        tick_upper,
        liquidity_delta,
    }
}

/// The post-apply dump: `(pool_id, positions, init_maps, marker_block, marker_log_idx)`.
type Dbdump = (
    i64,
    Vec<(i32, String, String)>,
    Vec<(i64, String)>,
    Option<i64>,
    Option<i64>,
);

/// Dump the actual post-apply rows from the DB (mirrors the Python generator's
/// `_dump_v3_state` / `_dump_v4_state` shape so the comparison is like-for-like).
fn dump_v3(db: &DegenbotDb) -> Dbdump {
    let conn = db.lock();
    let row: (i64, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT pool_id, liquidity_update_block, liquidity_update_log_index \
             FROM uniswap_v3_pools LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let pool_id = row.0;
    let mut positions: Vec<(i32, String, String)> = conn
        .prepare("SELECT tick, liquidity_net, liquidity_gross FROM liquidity_positions WHERE pool_id = ?1")
        .unwrap()
        .query_map(rusqlite::params![pool_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    positions.sort_by_key(|p| p.0);
    let mut init_maps: Vec<(i64, String)> = conn
        .prepare("SELECT word, bitmap FROM initialization_maps WHERE pool_id = ?1")
        .unwrap()
        .query_map(rusqlite::params![pool_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    init_maps.sort_by_key(|p| p.0);
    (pool_id, positions, init_maps, row.1, row.2)
}

fn dump_v4(db: &DegenbotDb) -> Dbdump {
    let conn = db.lock();
    let row: (i64, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT managed_pool_id, liquidity_update_block, liquidity_update_log_index \
             FROM uniswap_v4_pools LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let managed_pool_id = row.0;
    let mut positions: Vec<(i32, String, String)> = conn
        .prepare(
            "SELECT tick, liquidity_net, liquidity_gross FROM managed_pool_liquidity_positions \
             WHERE managed_pool_id = ?1",
        )
        .unwrap()
        .query_map(rusqlite::params![managed_pool_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    positions.sort_by_key(|p| p.0);
    let mut init_maps: Vec<(i64, String)> = conn
        .prepare(
            "SELECT word, bitmap FROM managed_pool_initialization_maps WHERE managed_pool_id = ?1",
        )
        .unwrap()
        .query_map(rusqlite::params![managed_pool_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    init_maps.sort_by_key(|p| p.0);
    (managed_pool_id, positions, init_maps, row.1, row.2)
}

fn expected_for(name: &str) -> Expected {
    let path = PathBuf::from(FIXTURE_DIR).join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[test]
fn v3_apply_matches_python_oracle() {
    let tmp = copy_fixture_to_tmp("liquidity_updater_v3_initial.db", "v3");
    let (db, state) = DegenbotDb::open_for_writes(&tmp).unwrap();
    assert_eq!(state, SchemaState::AlembicCurrent);

    let exp = expected_for("liquidity_updater_v3_expected.json");

    // Sanity: the pool has no marker before the apply.
    let conn = db.lock();
    let pre_marker: (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT liquidity_update_block, liquidity_update_log_index FROM uniswap_v3_pools LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    drop(conn);
    assert_eq!(
        pre_marker,
        (None, None),
        "fixture pre-apply marker must be None"
    );

    db.apply_v3_liquidity_updates(
        exp.chain_id,
        &exp.pool_address,
        &v3_events().into_iter().map(to_event).collect::<Vec<_>>(),
    )
    .unwrap();

    let (_pool_id, positions, init_maps, block, log_idx) = dump_v3(&db);
    assert_eq!(
        positions,
        to_position_tuple(&exp.positions),
        "V3 positions must match the Python oracle"
    );
    assert_eq!(
        init_maps,
        to_initmap_tuple(&exp.initialization_maps),
        "V3 init maps must match the Python oracle"
    );
    assert_eq!(
        block, exp.liquidity_update_block,
        "V3 marker block must match"
    );
    assert_eq!(
        log_idx, exp.liquidity_update_log_index,
        "V3 marker log_index must match"
    );
}

#[test]
fn v4_apply_matches_python_oracle() {
    let tmp = copy_fixture_to_tmp("liquidity_updater_v4_initial.db", "v4");
    let (db, state) = DegenbotDb::open_for_writes(&tmp).unwrap();
    assert_eq!(state, SchemaState::AlembicCurrent);

    let exp = expected_for("liquidity_updater_v4_expected.json");

    let conn = db.lock();
    let pre_marker: (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT liquidity_update_block, liquidity_update_log_index FROM uniswap_v4_pools LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    drop(conn);
    assert_eq!(
        pre_marker,
        (None, None),
        "fixture pre-apply marker must be None"
    );

    db.apply_v4_liquidity_updates(
        &exp.pool_hash,
        exp.pool_manager_chain,
        &v4_events().into_iter().map(to_event).collect::<Vec<_>>(),
    )
    .unwrap();

    let (_managed_pool_id, positions, init_maps, block, log_idx) = dump_v4(&db);
    assert_eq!(
        positions,
        to_position_tuple(&exp.positions),
        "V4 positions must match the Python oracle"
    );
    assert_eq!(
        init_maps,
        to_initmap_tuple(&exp.initialization_maps),
        "V4 init maps must match the Python oracle"
    );
    assert_eq!(
        block, exp.liquidity_update_block,
        "V4 marker block must match"
    );
    assert_eq!(
        log_idx, exp.liquidity_update_log_index,
        "V4 marker log_index must match"
    );
}

/// Convert a Uniswap V3 fixture pool to a `PancakeSwap` V3 fork: move the
/// subclass row from `uniswap_v3_pools` to `pancakeswap_v3_pools` and flip
/// `pools.kind`. The `liquidity_positions` / `initialization_maps` rows stay
/// (they're keyed by `pool_id`, kind-agnostic). This isolates the bug: the Rust
/// updater must resolve the subclass-table at runtime, not hardcode
/// `uniswap_v3_pools`.
fn relabel_pool_as_pancakeswap_v3(conn: &rusqlite::Connection) -> i64 {
    // Read the uniswap_v3_pools row, then replicate it into the
    // pancakeswap_v3_pools table, drop the original, and change pools.kind.
    let (pool_id, tick_spacing, lub, lui, ft0, ft1, fd): (
        i64,
        i64,
        Option<i64>,
        Option<i64>,
        i64,
        i64,
        i64,
    ) = conn
        .query_row(
            "SELECT pool_id, tick_spacing, liquidity_update_block, \
             liquidity_update_log_index, fee_token0, fee_token1, fee_denominator \
             FROM uniswap_v3_pools LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .unwrap();
    conn.execute(
        "INSERT INTO pancakeswap_v3_pools \
         (pool_id, tick_spacing, liquidity_update_block, liquidity_update_log_index, \
          fee_token0, fee_token1, fee_denominator) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![pool_id, tick_spacing, lub, lui, ft0, ft1, fd],
    )
    .unwrap();
    conn.execute(
        "DELETE FROM uniswap_v3_pools WHERE pool_id = ?1",
        rusqlite::params![pool_id],
    )
    .unwrap();
    conn.execute(
        "UPDATE pools SET kind = 'pancakeswap_v3' WHERE id = ?1",
        rusqlite::params![pool_id],
    )
    .unwrap();
    pool_id
}

/// Dump the post-apply marker from the given subclass table (used to verify
/// the stamp landed on the FORK table, not `uniswap_v3_pools`).
fn dump_marker_from(
    conn: &rusqlite::Connection,
    table: &str,
    pool_id: i64,
) -> (Option<i64>, Option<i64>) {
    conn.query_row(
        &format!(
            "SELECT liquidity_update_block, liquidity_update_log_index FROM {table} WHERE pool_id = ?1"
        ),
        rusqlite::params![pool_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap_or((None, None))
}

#[test]
fn v3_fork_pool_apply_matches_oracle() {
    // Regression test for the subclass-table bug: the V3 liquidity updater
    // hardcoded `uniswap_v3_pools` in `fetch_v3_pool_update_state_on_conn` and
    // `set_v3_liquidity_update_marker_on_conn`, silently no-oping for every
    // non-Uniswap V3 fork. Re-label the fixture's pool as `pancakeswap_v3`
    // (same seed, same events, same oracle — only the subclass-table changes)
    // and assert the apply still writes positions + stamps the marker on the
    // FORK table.
    let tmp = copy_fixture_to_tmp("liquidity_updater_v3_initial.db", "v3fork");
    let (db, state) = DegenbotDb::open_for_writes(&tmp).unwrap();
    assert_eq!(state, SchemaState::AlembicCurrent);

    let exp = expected_for("liquidity_updater_v3_expected.json");

    // Relabel: move uniswap_v3_pools row → pancakeswap_v3_pools, flip kind.
    let conn = db.lock();
    let pool_id = relabel_pool_as_pancakeswap_v3(&conn);

    // Sanity: uniswap_v3_pools no longer has the row, but pancakeswap_v3_pools does.
    let uni_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM uniswap_v3_pools WHERE pool_id = ?1",
            rusqlite::params![pool_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(uni_count, 0, "uniswap_v3_pools row must be removed");
    let cake_row: (Option<i64>, Option<i64>) =
        dump_marker_from(&conn, "pancakeswap_v3_pools", pool_id);
    assert_eq!(cake_row, (None, None), "pre-apply fork marker must be None");
    drop(conn);

    let applied = db
        .apply_v3_liquidity_updates(
            exp.chain_id,
            &exp.pool_address,
            &v3_events().into_iter().map(to_event).collect::<Vec<_>>(),
        )
        .unwrap();

    assert!(
        applied,
        "apply_v3_liquidity_updates must return true for a pancakeswap_v3 pool"
    );

    let conn = db.lock();
    // Positions + init-maps are kind-agnostic (keyed by pool_id).
    let mut positions: Vec<(i32, String, String)> = conn
        .prepare("SELECT tick, liquidity_net, liquidity_gross FROM liquidity_positions WHERE pool_id = ?1")
        .unwrap()
        .query_map(rusqlite::params![pool_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    positions.sort_by_key(|p| p.0);
    let mut init_maps: Vec<(i64, String)> = conn
        .prepare("SELECT word, bitmap FROM initialization_maps WHERE pool_id = ?1")
        .unwrap()
        .query_map(rusqlite::params![pool_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    init_maps.sort_by_key(|p| p.0);

    assert_eq!(
        positions,
        to_position_tuple(&exp.positions),
        "fork V3 positions must match oracle"
    );
    assert_eq!(
        init_maps,
        to_initmap_tuple(&exp.initialization_maps),
        "fork V3 init-maps must match oracle"
    );

    // The marker must be on pancakeswap_v3_pools — NOT on uniswap_v3_pools.
    let fork_marker = dump_marker_from(&conn, "pancakeswap_v3_pools", pool_id);
    assert_eq!(
        fork_marker.0, exp.liquidity_update_block,
        "fork marker block must match"
    );
    assert_eq!(
        fork_marker.1, exp.liquidity_update_log_index,
        "fork marker log_index must match"
    );

    // And uniswap_v3_pools must NOT have a row (would indicate a write to the wrong table).
    let uni_marker = dump_marker_from(&conn, "uniswap_v3_pools", pool_id);
    assert_eq!(
        uni_marker,
        (None, None),
        "no marker on uniswap_v3_pools for a pancakeswap_v3 pool"
    );
}

fn to_position_tuple(rows: &[PositionRow]) -> Vec<(i32, String, String)> {
    let mut out: Vec<(i32, String, String)> = rows
        .iter()
        .map(|r| (r.tick, r.liquidity_net.clone(), r.liquidity_gross.clone()))
        .collect();
    out.sort_by_key(|p| p.0);
    out
}

fn to_initmap_tuple(rows: &[InitMapRow]) -> Vec<(i64, String)> {
    let mut out: Vec<(i64, String)> = rows.iter().map(|r| (r.word, r.bitmap.clone())).collect();
    out.sort_by_key(|p| p.0);
    out
}

// Keep U256 import live (used implicitly via the JSON comparison of bitmap
// decimal strings — the literals are compared as strings, but U256 is the
// type the Rust side decodes to + re-encodes for the comparison).
#[allow(unused_imports)]
use U256 as _U256;
