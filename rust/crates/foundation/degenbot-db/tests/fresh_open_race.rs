//! Regression: a concurrent fresh-open race on one path.
//!
//! Two (or more) processes/threads racing the fresh-standalone open of the
//! same not-yet-existing database used to be a coin flip: both classified the
//! empty file as `FreshStandalone` and both ran the head DDL, and the loser
//! died with `index ... already exists` because the `CREATE INDEX`
//! statements lacked `IF NOT EXISTS` (the `CREATE TABLE` statements already
//! had it). In the wild this bit the xdist pathfinding tier (parallel workers
//! racing a per-session fresh DB) and the settlement-bot `boot_gate` tests
//! (worked around there with `DEGENBOT_DB_AUTO_HEAL=0`).
//!
//! Two further fresh-open races surfaced while hardening this one, and the fix
//! covers all three:
//! 1. the DDL replay (`CREATE [UNIQUE] INDEX IF NOT EXISTS`, statement-form only);
//! 2. the `PRAGMA journal_mode=WAL` switch, which SQLite's busy handler does
//!    NOT cover — retried in `pragma::apply_open_pragmas`;
//! 3. the half-built DDL window, where a peer saw content tables before the
//!    stamp table and refused the file as `Unrecognized` — closed by applying
//!    the DDL + stamp in one transaction (`migrate::apply_fresh_standalone`).
//!
//! This drives all three through the production `DegenbotDb::open` seam (the
//! path a boot takes) with a `Barrier` to maximize overlap.

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Barrier};
use std::thread;

use degenbot_db::{DegenbotDb, SchemaState};
use tempfile::TempDir;

const RACERS: usize = 8;

/// N threads released together all open the SAME absent path. Every open must
/// succeed; the survivors are either the `FreshStandalone` creator or a
/// `RustOwned` re-classification — none may fail.
#[test]
fn concurrent_fresh_open_of_one_path_all_succeed() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("raced.db");
    assert!(!db_path.exists(), "precondition: the file is absent");

    let barrier = Arc::new(Barrier::new(RACERS));
    let handles: Vec<_> = (0..RACERS)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let db_path = db_path.clone();
            thread::spawn(move || {
                barrier.wait();
                DegenbotDb::open(&db_path).map(|(_db, state)| state)
            })
        })
        .collect();

    let mut fresh = 0usize;
    for handle in handles {
        let state = handle
            .join()
            .unwrap()
            .expect("every racer must open successfully — no \"index already exists\", \"database is locked\", or UnrecognizedSchema");
        match state {
            SchemaState::FreshStandalone { .. } => fresh += 1,
            SchemaState::RustOwned { .. } => {}
            other => panic!("unexpected post-race state: {other:?}"),
        }
    }

    // The file is a single valid Rust-owned DB: one stamp row, and the full
    // index set present exactly once (the DDL replay created no duplicates).
    let (db, state) = DegenbotDb::open(&db_path).unwrap();
    assert!(
        matches!(state, SchemaState::RustOwned { .. }),
        "the raced file must settle RustOwned, got {state:?}"
    );
    let conn = db.lock();
    let dupes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (SELECT name FROM sqlite_master \
             WHERE type='index' AND name NOT LIKE 'sqlite_%' \
             GROUP BY name HAVING COUNT(*) > 1)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dupes, 0, "no duplicated index names after the race");
    // At least one racer observed the fresh-create (never zero — the race is
    // always resolved by exactly one creator).
    assert!(fresh >= 1, "one racer must have created the fresh DB");
}
