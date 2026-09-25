//! The open path is the database's owner: the first open of a resolved path
//! creates the file, its parent directory, and the full Rust schema; a second
//! open is a no-op; and the pre-existing dispositions (empty file, Alembic
//! stamp) reach the ADR-052 head at open.
//!
//! These exercise the public [`DegenbotDb::open`] seam rather than the
//! `ops::create_new_database` admin verb, because a boot's database comes into
//! existence through an open, not through a separate create call the driver
//! would have to remember to make.

#![expect(clippy::unwrap_used)]

use degenbot_db::schema::RUST_SCHEMA_VERSION;
use degenbot_db::{DegenbotDb, SchemaState};
use rusqlite::Connection;
use tempfile::TempDir;

/// The private Rust-owned schema stamp table (not re-exported as a constant).
const RUST_STAMP_TABLE: &str = "_degenbot_db_schema_version";

fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        rusqlite::params![name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

fn rust_stamp(conn: &Connection) -> i64 {
    conn.query_row(
        &format!("SELECT schema_version FROM {RUST_STAMP_TABLE}"),
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn bak_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap().to_owned();
    name.push(".bak");
    path.with_file_name(name)
}

/// A boot resolves a state-home path several directories deep under a state
/// root that does not exist yet. The first open must materialize the parent,
/// the file, and the schema; the second must find it already Rust-owned.
#[test]
fn first_open_creates_the_database_and_a_second_open_is_a_noop() {
    let dir = TempDir::new().unwrap();
    let db_path = dir
        .path()
        .join("state")
        .join("degenbot")
        .join("db")
        .join("degenbot.db");

    assert!(!db_path.exists(), "precondition: the file is absent");
    assert!(
        !db_path.parent().unwrap().exists(),
        "precondition: the parent directory is absent too"
    );

    let (db, state) = DegenbotDb::open(&db_path).unwrap();
    assert!(
        matches!(state, SchemaState::FreshStandalone { .. }),
        "the first open of an absent path is a fresh standalone, got {state:?}"
    );
    assert!(db_path.is_file(), "the first open must create the file");
    {
        let conn = db.lock();
        for table in ["erc20_tokens", "exchanges", "pools", "liquidity_positions"] {
            assert!(
                has_table(&conn, table),
                "{table} missing after the first open"
            );
        }
        assert!(
            has_table(&conn, RUST_STAMP_TABLE),
            "the first open must stamp the Rust schema"
        );
        assert_eq!(rust_stamp(&conn), i64::from(RUST_SCHEMA_VERSION));
        assert!(
            !has_table(&conn, "alembic_version"),
            "a fresh Rust-owned DB carries no Alembic marker"
        );
    }
    drop(db);

    let (_db, second) = DegenbotDb::open(&db_path).unwrap();
    assert!(
        matches!(second, SchemaState::RustOwned { .. }),
        "the second open must find the database already Rust-owned, got {second:?}"
    );
    assert!(
        !bak_path(&db_path).exists(),
        "a Rust-owned reopen must not take a heal backup"
    );
}

/// An existing-but-empty file is a fresh standalone: the open applies the
/// embedded head DDL and the Rust stamp.
#[test]
fn open_brings_an_existing_empty_file_up_to_the_schema() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("empty.db");
    std::fs::write(&db_path, b"").unwrap();

    let (db, state) = DegenbotDb::open(&db_path).unwrap();
    assert!(matches!(state, SchemaState::FreshStandalone { .. }));
    let conn = db.lock();
    assert!(has_table(&conn, "pools"));
    assert!(has_table(&conn, RUST_STAMP_TABLE));
    assert_eq!(rust_stamp(&conn), i64::from(RUST_SCHEMA_VERSION));
}

/// An Alembic-stamped file heals out-of-place at open (ADR-052 D1), leaving the
/// pre-heal file as the recoverable `.bak`.
#[test]
fn open_heals_an_alembic_stamped_file() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("legacy.db");
    degenbot_db::create_new_database(&db_path).unwrap();
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "DROP TABLE _degenbot_db_schema_version;\n\
             CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
             INSERT INTO alembic_version (version_num) VALUES ('e0aaad8ad486');",
        )
        .unwrap();
    }

    let (db, state) = DegenbotDb::open(&db_path).unwrap();
    assert!(
        matches!(state, SchemaState::RustOwned { .. }),
        "the Alembic-stamped file must heal to Rust-owned, got {state:?}"
    );
    let conn = db.lock();
    assert!(!has_table(&conn, "alembic_version"));
    assert!(has_table(&conn, RUST_STAMP_TABLE));
    drop(conn);
    assert!(
        bak_path(&db_path).exists(),
        "the pre-heal database must survive as `.bak`"
    );
}
