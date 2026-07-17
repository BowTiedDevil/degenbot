//! Regression test for the WAL held-read-tx concurrency model (spike
//! `HKJ7VR` / epic `XEANMB`): `DegenbotDb::open_snapshot_tx()` acquires one
//! deferred read transaction at bot startup and holds it across `build_paths`
//! so every per-pool `fetch_liquidity_map` + the `fetch_newest_update_block`
//! read share a single DB snapshot, immune to concurrent `pool_updater`
//! commits (Approach 1, chosen method — see
//! `docs/architecture/snapshot-store-removal-scoping.md` §2).
//!
//! This is the behavior the `SnapshotStore` was hand-rolling: a single
//! consistent DB cut frozen at bot startup that concurrent writer commits
//! cannot perturb, for the lifetime of `build_paths`. WAL snapshot isolation
//! replaces the in-memory store with a primitive `SQLite` already provides.
//!
//! Setup mirrors the bot's deployment shape:
//! - one **reader** connection (the bot's `DegenbotDb`: WAL + `query_only=on`)
//! - one **writer** connection (the separate `pool_updater` process: WAL,
//!   not read-only) to the SAME file.
//!
//! Assertions (the race the bot cares about):
//! 1. Inside one held deferred read tx on the reader, two `last_update_block`
//!    reads see the SAME (pre-write) block, even though the writer advanced it
//!    to a higher block between them. (A naive per-call `lock()` would see
//!    the advanced block on the second read — the false-verify-failure race.)
//! 2. After the held tx commits, a fresh read sees the writer's advanced block
//!    (the snapshot released — subsequent boots/registrations see live data).
//! 3. Tick-data rows read inside the held tx also reflect the pre-write snapshot
//!    (not just the scalar block number).
//!
//! The negative-control test (`per_call_reads_see_concurrent_writer_advance`)
//! pins the race the held tx closes: without a held tx (today's per-call
//! `lock()` shape), the second read sees the writer's advance — the
//! false-verify-failure the Store was working around.

use rusqlite::{Connection, TransactionBehavior};
use tempfile::TempDir;

/// Replicate the project's read-connection PRAGMAs (`degenbot-db/src/connection.rs`).
fn pr_ok() -> &'static str {
    "PRAGMA busy_timeout=5000;\nPRAGMA synchronous=NORMAL;"
}

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS exchanges (
    id INTEGER NOT NULL, chain_id INTEGER NOT NULL, name TEXT NOT NULL,
    active BOOLEAN NOT NULL, last_update_block INTEGER,
    factory VARCHAR(42) NOT NULL, deployer VARCHAR(42),
    PRIMARY KEY (id));
CREATE TABLE IF NOT EXISTS pools (
    id INTEGER NOT NULL, address VARCHAR(42) NOT NULL, chain INTEGER NOT NULL,
    kind TEXT NOT NULL, token0_id INTEGER, token1_id INTEGER, exchange_id INTEGER NOT NULL,
    PRIMARY KEY (id));
CREATE TABLE IF NOT EXISTS liquidity_positions (
    id INTEGER NOT NULL, pool_id INTEGER NOT NULL, tick INTEGER NOT NULL,
    liquidity_net VARCHAR(78) NOT NULL, liquidity_gross VARCHAR(78) NOT NULL,
    PRIMARY KEY (id));
";

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "behavior test — split hurts the positive+negative narrative"
)]
fn held_read_transaction_freezes_view_across_concurrent_writer_commits() {
    let dir = TempDir::new().expect("tmpdir");
    let db_path = dir.path().join("bot.sqlite");

    // ---- writer connection (the pool_updater process) --------------------
    let writer = Connection::open(&db_path).expect("open writer");
    writer
        .execute_batch("PRAGMA journal_mode=WAL;")
        .expect("wal");
    writer.execute_batch(pr_ok()).expect("pragmas");
    writer.execute_batch(SCHEMA).expect("schema");

    // Seed chain 1, one V3 exchange at block 100, one pool, one tick row.
    writer
        .execute(
            "INSERT INTO exchanges (id, chain_id, name, active, last_update_block, factory, deployer) \
             VALUES (1, 1, 'uniswap_v3', 1, 100, '0x1F98431c8aD98523631AE4a59f267346ea31F984', NULL)",
            [],
        )
        .expect("insert exchange");
    writer
        .execute(
            "INSERT INTO pools (id, address, chain, kind, token0_id, token1_id, exchange_id) \
             VALUES (10, '0xPool', 1, 'uniswap_v3', NULL, NULL, 1)",
            [],
        )
        .expect("insert pool");
    writer
        .execute(
            "INSERT INTO liquidity_positions (id, pool_id, tick, liquidity_net, liquidity_gross) \
             VALUES (1, 10, -100, '1000', '1000')",
            [],
        )
        .expect("insert tick");
    writer.execute_batch("COMMIT;").ok(); // finalize any implicit tx

    // ---- reader connection (the bot's DegenbotDb) ------------------------
    let mut reader = Connection::open(&db_path).expect("open reader");
    reader
        .execute_batch("PRAGMA journal_mode=WAL;")
        .expect("wal rd");
    reader.execute_batch(pr_ok()).expect("pragas rd");
    reader
        .pragma_update(None, "query_only", "on")
        .expect("query_only");

    // Self-document: this test exercises WAL. A different journal_mode would
    // change the reader/writer concurrency shape (DELETE mode blocks writers).
    let mode: String = reader
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("journal_mode");
    assert_eq!(
        mode, "wal",
        "WAL must be active for snapshot-isolation concurrency"
    );

    // Sanity: both connections see block 100 + the seeded tick before the race.
    let s_before: i64 = reader
        .query_row(
            "SELECT COALESCE(MIN(last_update_block),0) FROM exchanges WHERE chain_id=1",
            [],
            |r| r.get(0),
        )
        .expect("read s_before");
    assert_eq!(s_before, 100, "pre-write: both see block 100");
    let tick_before: i64 = reader
        .query_row(
            "SELECT COUNT(*) FROM liquidity_positions WHERE pool_id=10",
            [],
            |r| r.get(0),
        )
        .expect("count before");
    assert_eq!(tick_before, 1, "pre-write: one tick row");

    // === the held deferred read tx (Approach 1) ===========================
    // `BEGIN DEFERRED` → the snapshot is established at the FIRST read inside.
    let tx = reader
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .expect("begin deferred tx");

    // First read establishes the WAL snapshot at block 100.
    let s_in_tx_1: i64 = tx
        .query_row(
            "SELECT COALESCE(MIN(last_update_block),0) FROM exchanges WHERE chain_id=1",
            [],
            |r| r.get(0),
        )
        .expect("read s_in_tx_1");
    assert_eq!(s_in_tx_1, 100, "inside held tx (1st read): snapshot at 100");

    // ---- concurrent writer commits: advance the exchange to block 110 -----
    writer.execute_batch("BEGIN;").expect("w begin");
    writer
        .execute(
            "UPDATE exchanges SET last_update_block = 110 WHERE id = 1",
            [],
        )
        .expect("w advance block");
    writer
        .execute("INSERT INTO liquidity_positions (id, pool_id, tick, liquidity_net, liquidity_gross) VALUES (2, 10, 50, '2000', '2000')", [])
        .expect("w insert tick");
    writer.execute_batch("COMMIT;").expect("w commit");

    // Confirm the writer's new state is visible to a FRESH connection (not the held tx).
    let live: i64 = Connection::open(&db_path)
        .expect("fresh conn")
        .query_row(
            "SELECT last_update_block FROM exchanges WHERE id=1",
            [],
            |r| r.get(0),
        )
        .expect("fresh read");
    assert_eq!(
        live, 110,
        "fresh connection sees writer's committed block 110"
    );

    // ---- assertions 1 + 3: the held tx's view is FROZEN ------------------
    let s_in_tx_2: i64 = tx
        .query_row(
            "SELECT COALESCE(MIN(last_update_block),0) FROM exchanges WHERE chain_id=1",
            [],
            |r| r.get(0),
        )
        .expect("read s_in_tx_2");
    assert_eq!(
        s_in_tx_2, 100,
        "held tx (2nd read): STILL sees block 100 despite writer advancing to 110"
    );

    let ticks_in_tx: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM liquidity_positions WHERE pool_id=10",
            [],
            |r| r.get(0),
        )
        .expect("count in tx");
    assert_eq!(
        ticks_in_tx, 1,
        "held tx: STILL sees 1 tick row (writer's 2nd row invisible)"
    );

    tx.commit().expect("commit held tx");

    // ---- assertion 2: after commit, a fresh read sees the live DB ---------
    let s_after: i64 = reader
        .query_row(
            "SELECT COALESCE(MIN(last_update_block),0) FROM exchanges WHERE chain_id=1",
            [],
            |r| r.get(0),
        )
        .expect("read s_after");
    assert_eq!(s_after, 110, "post-commit: reader now sees block 110");

    let ticks_after: i64 = reader
        .query_row(
            "SELECT COUNT(*) FROM liquidity_positions WHERE pool_id=10",
            [],
            |r| r.get(0),
        )
        .expect("count after");
    assert_eq!(ticks_after, 2, "post-commit: reader now sees 2 tick rows");
}

/// Negative control: WITHOUT a held tx (mirroring today's per-call `lock()`
/// shape where every read is its own auto-commit snapshot), the second read
/// sees the writer's committed advance. This is the false-verify-failure race
/// the Store was hand-rolling around — the held-tx test above closes it.
#[test]
fn per_call_reads_see_concurrent_writer_advance() {
    let dir = TempDir::new().expect("tmpdir");
    let db_path = dir.path().join("bot2.sqlite");

    let writer = Connection::open(&db_path).expect("open writer");
    writer
        .execute_batch("PRAGMA journal_mode=WAL;\nPRAGMA busy_timeout=5000;")
        .expect("wal");
    writer.execute_batch(SCHEMA).expect("schema");
    writer
        .execute(
            "INSERT INTO exchanges (id, chain_id, name, active, last_update_block, factory, deployer) \
             VALUES (1, 1, 'uniswap_v3', 1, 100, '0xF', NULL)",
            [],
        )
        .expect("insert exchange");
    writer.execute_batch("COMMIT;").ok();

    let reader = Connection::open(&db_path).expect("open reader");
    reader
        .execute_batch("PRAGMA journal_mode=WAL;\nPRAGMA busy_timeout=5000;")
        .expect("wal rd");
    reader.pragma_update(None, "query_only", "on").expect("ro");

    // Read 1 — auto-commit, sees block 100.
    let s1: i64 = reader
        .query_row(
            "SELECT COALESCE(MIN(last_update_block),0) FROM exchanges WHERE chain_id=1",
            [],
            |r| r.get(0),
        )
        .expect("read s1");
    assert_eq!(s1, 100, "per-call read 1: sees block 100");

    // Writer advances.
    writer.execute_batch("BEGIN;").expect("w begin");
    writer
        .execute(
            "UPDATE exchanges SET last_update_block = 110 WHERE id = 1",
            [],
        )
        .expect("advance");
    writer.execute_batch("COMMIT;").expect("w commit");

    // Read 2 — a NEW auto-commit snapshot: sees the writer's 110.
    let s2: i64 = reader
        .query_row(
            "SELECT COALESCE(MIN(last_update_block),0) FROM exchanges WHERE chain_id=1",
            [],
            |r| r.get(0),
        )
        .expect("read s2");
    assert_eq!(
        s2, 110,
        "per-call read 2: sees writer's block 110 — the race (held tx would freeze at 100)"
    );
}
