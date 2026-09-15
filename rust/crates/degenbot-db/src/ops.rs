//! `SQLite` **file operations**: create / backup / compact / upgrade.
//!
//! These are write/admin operations — unlike the read handle
//! [`DegenbotDb`][crate::connection::DegenbotDb], whose connections are
//! `PRAGMA query_only=on`, these open their own raw [`Connection`]s, run the
//! full file lifecycle a standalone Rust consumer (`cargo add degenbot-db`)
//! needs, and never set `query_only`:
//!
//! - [`create_new_database`] — WAL mode + the full head DDL + `VACUUM` + the
//!   private `_degenbot_db_schema_version` stamp (Rust-owned from birth).
//! - [`backup_database`] — `sqlite3.Connection.backup` equivalent
//!   (rusqlite's online `Backup`), preserving the Python
//!   `backup_sqlite_database`'s `PRAGMA integrity_check` assertions on **both**
//!   the source and the destination.
//! - [`compact_database`] — `VACUUM`.
//! - [`upgrade_database`] — ensure the DB is at the current Rust schema; on a
//!   fresh file applies the head DDL + the Rust stamp; on an already-Rust-owned
//!   DB it is a no-op; a legacy `alembic_version`-marked DB is healed
//!   out-of-place to Rust ownership (ADR-011); a foreign file is **refused**.
//!
//! # Rust-owned from birth
//!
//! [`create_new_database`] stamps the private `_degenbot_db_schema_version`
//! table directly, so a Rust-created DB opens as
//! [`SchemaState::RustOwned`] and never masquerades as a legacy
//! `alembic_version`-marked file it did not create.

use std::path::Path;
use std::time::Duration;

use rusqlite::{backup, Connection};

pub use crate::error::DbError;
use crate::migrate::{
    classify_schema, convert_alembic_to_rust_owned as run_cutover_on_conn,
    stamp_rust_schema_version, SchemaState,
};
use crate::schema::SCHEMA_HEAD;

/// The per-connection PRAGMAs the admin ops assert up front: WAL (file-persistent)
/// + the concurrency trio (mirrors [`crate::connection`] / the Python open path).
///
/// These ops do **not** set `query_only=on`; they must write.
const ADMIN_PRAGMAS: &str = "PRAGMA journal_mode=WAL;\n\
                             PRAGMA busy_timeout=5000;\n\
                             PRAGMA synchronous=NORMAL;";

/// The outcome [`upgrade_database`] reports so the caller (CLI) can log
/// whether the DB was already current, freshly created, or healed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeOutcome {
    /// The DB was already stamped at the current Rust schema — no-op.
    AlreadyCurrent,
    /// The file was empty — the full head DDL was applied and the Rust stamp
    /// written.
    CreatedFresh,
    /// A legacy `alembic_version`-marked DB was healed out-of-place to Rust
    /// ownership (ADR-011).
    HealedLegacy,
}

/// Create a fresh degenbot `SQLite` DB at `path`.
///
/// Runs, in order: WAL mode + concurrency PRAGMAs; `PRAGMA auto_vacuum=FULL`
/// (before any tables — only effective on a fresh DB, mirroring Python); the
/// full head DDL ([`SCHEMA_HEAD`]); `VACUUM`; then the private
/// `_degenbot_db_schema_version` Rust stamp. The result opens as
/// [`SchemaState::RustOwned`] — Rust-owned from birth.
///
/// # Errors
///
/// [`DbError::Sqlite`] on any connection/PRAGMA/DDL/stamp failure.
pub fn create_new_database(path: &Path) -> Result<(), DbError> {
    let conn = open_raw(path)?;
    conn.execute_batch(ADMIN_PRAGMAS)?;
    // auto_vacuum must be set before any tables are created; FULL only takes
    // effect on a fresh DB (`SQLite` ignores it otherwise — same as Python).
    conn.execute_batch("PRAGMA auto_vacuum=FULL;")?;
    conn.execute_batch(SCHEMA_HEAD)?;
    conn.execute_batch("VACUUM;")?;
    stamp_rust_schema_version(&conn)?;
    Ok(())
}

/// Back up `src` into `dst` via `SQLite`'s online backup, asserting
/// `PRAGMA integrity_check == "ok"` on **both** the source (before) and the
/// destination (after) — preserving the Python `backup_sqlite_database`
/// assertions verbatim.
///
/// `dst` is created if absent and overwritten if present.
///
/// # Errors
///
/// [`DbError::Sqlite`] on an open/backup failure;
/// [`DbError::IntegrityCheckFailed`] if either integrity check is not `"ok"`.
pub fn backup_database(src: &Path, dst: &Path) -> Result<(), DbError> {
    let src_conn = Connection::open(src)?;
    assert_integrity_ok(&src_conn)?;

    let mut dst_conn = Connection::open(dst)?;
    {
        let backup = backup::Backup::new(&src_conn, &mut dst_conn)?;
        backup.run_to_completion(100, Duration::from_millis(250), None)?;
    } // dst_handle dropped here — flushes + closes the backup handle.

    // Re-open the destination for the post-backup integrity check so it is
    // fully settled on disk (a fresh connection sees a consistent file).
    drop(dst_conn);
    let dst_check = Connection::open(dst)?;
    assert_integrity_ok(&dst_check)?;
    Ok(())
}

/// Compact `path` via `VACUUM`. A no-op (returns `Ok`) for `:memory:`, matching
/// the Python `compact_sqlite_database` skip.
///
/// # Errors
///
/// [`DbError::Sqlite`] if the connection or `VACUUM` fails.
pub fn compact_database(path: &Path) -> Result<(), DbError> {
    if path == Path::new(":memory:") {
        return Ok(());
    }
    let conn = open_raw(path)?;
    conn.execute_batch("VACUUM;")?;
    Ok(())
}

/// Ensure `path` is at the current Rust schema.
///
/// - A Rust-owned DB at the current stamp → no-op
///   ([`UpgradeOutcome::AlreadyCurrent`]).
/// - An empty file (no tables, no legacy history) → applies the full head DDL
///   + the Rust stamp ([`UpgradeOutcome::CreatedFresh`]).
/// - A legacy `alembic_version`-marked DB → healed out-of-place to Rust
///   ownership via [`crate::heal::heal_database`]
///   ([`UpgradeOutcome::HealedLegacy`]).
/// - A foreign file (tables present, neither Rust-owned nor legacy) →
///   [`DbError::UnrecognizedSchema`].
///
/// # Errors
///
/// See above; [`DbError::Sqlite`] on any I/O / SQL failure.
pub fn upgrade_database(path: &Path) -> Result<UpgradeOutcome, DbError> {
    let conn = open_raw(path)?;
    conn.execute_batch(ADMIN_PRAGMAS)?;

    let state = classify_schema(&conn)?;
    match state {
        SchemaState::FreshStandalone { .. } => {
            conn.execute_batch(SCHEMA_HEAD)?;
            stamp_rust_schema_version(&conn)?;
            Ok(UpgradeOutcome::CreatedFresh)
        }
        SchemaState::RustOwned { .. } => Ok(UpgradeOutcome::AlreadyCurrent),
        SchemaState::LegacyAlembic => {
            // The heal renames the file (atomic swap): drop this handle first.
            drop(conn);
            heal_database(path)?;
            Ok(UpgradeOutcome::HealedLegacy)
        }
        SchemaState::Unrecognized => Err(DbError::UnrecognizedSchema),
    }
}

/// Inspect `path`'s schema state WITHOUT writing (ADR-010 §2). The read-only
/// dry-run companion to [`upgrade_database`] / [`convert_alembic_to_rust_owned`]:
/// runs [`classify_schema`] (pure predicates, NO DDL — even the
/// `FreshStandalone` arm reports the would-be state without applying tables)
/// and returns the [`SchemaState`] for ALL cases, including
/// `LegacyAlembic` / `Unrecognized` (it never refuses — the
/// `database cutover --dry-run` command reports the state to the user rather
/// than raising).
///
/// # Errors
///
/// [`DbError::Sqlite`] on an open / query failure. Never refuses on schema
/// disposition.
pub fn inspect_schema_state(path: &Path) -> Result<SchemaState, DbError> {
    let conn = open_raw(path)?;
    conn.execute_batch(ADMIN_PRAGMAS)?;
    classify_schema(&conn)
}

// Re-export the out-of-place heal (ADR-011) so consumers of `ops` find every
// admin file operation in one place. The implementation lives in [`crate::heal`].
pub use crate::heal::{heal_database, HealReport};

/// The opt-in one-way cutover (ADR-010 §1+§2): flip a legacy
/// `alembic_version`-marked DB into Rust ownership. Runs
/// [`migrate::convert_alembic_to_rust_owned`] on a raw admin connection (drops
/// the marker table, stamps `_degenbot_db_schema_version`), then reads the
/// resulting state back via [`classify_schema`] (→ `RustOwned`).
///
/// Refuses [`SchemaState::Unrecognized`] (foreign file →
/// `Err(DbError::UnrecognizedSchema)`). Already-`RustOwned` (and
/// `FreshStandalone`) DBs are an idempotent re-stamp no-op.
///
/// # Errors
///
/// See [`DbError::UnrecognizedSchema`] / [`DbError::Sqlite`].
pub fn convert_alembic_to_rust_owned(path: &Path) -> Result<SchemaState, DbError> {
    let conn = open_raw(path)?;
    conn.execute_batch(ADMIN_PRAGMAS)?;
    run_cutover_on_conn(&conn)?;
    classify_schema(&conn)
}

/// Open a writable raw [`Connection`] to `path` (`:memory:` supported), with no
/// `query_only` set. These admin ops must write.
fn open_raw(path: &Path) -> Result<Connection, DbError> {
    if path == Path::new(":memory:") {
        Connection::open_in_memory()
    } else {
        Connection::open(path)
    }
    .map_err(Into::into)
}

/// Assert `PRAGMA integrity_check` is `"ok"`, mirroring the Python
/// `backup_sqlite_database` assertions.
fn assert_integrity_ok(conn: &Connection) -> Result<(), DbError> {
    let result: String = conn.query_row("PRAGMA integrity_check;", [], |r| r.get(0))?;
    if result == "ok" {
        Ok(())
    } else {
        Err(DbError::IntegrityCheckFailed(result))
    }
}

/// `true` if a table named `name` exists in `conn`'s `sqlite_master`
/// (mirrors the private helper in [`crate::migrate`]). Test-only: the
/// production `upgrade_database` path classifies via [`classify_schema`].
#[cfg(test)]
fn table_exists(conn: &Connection, name: &str) -> Result<bool, DbError> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        rusqlite::params![name],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}
#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::connection::DegenbotDb;
    use crate::migrate::SchemaState;
    use crate::schema::RUST_SCHEMA_VERSION;

    /// The private Rust-owned schema stamp table (not re-exported as a constant).
    const RUST_STAMP_TABLE: &str = "_degenbot_db_schema_version";

    /// Flip a Rust-owned DB to the legacy `alembic_version`-marked shape.
    fn mark_legacy(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(&format!(
            "DROP TABLE {RUST_STAMP_TABLE};\n\
             CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
             INSERT INTO alembic_version (version_num) VALUES ('e0aaad8ad486');"
        ))
        .unwrap();
    }

    #[test]
    fn create_new_database_stamps_rust_schema_and_classifies_owned() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("fresh.db");
        create_new_database(&db_path).unwrap();

        // wal mode set on the file
        let probe = Connection::open(&db_path).unwrap();
        let jm: String = probe
            .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(jm, "wal");

        // the private Rust stamp is written; no legacy marker exists
        let stamp: i64 = probe
            .query_row(
                &format!("SELECT schema_version FROM {RUST_STAMP_TABLE}"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamp, i64::from(RUST_SCHEMA_VERSION));
        assert!(!table_exists(&probe, "alembic_version").unwrap());

        // every core table present
        for t in ["exchanges", "erc20_tokens", "pools", "liquidity_positions"] {
            assert!(table_exists(&probe, t).unwrap(), "{t} missing");
        }

        assert_eq!(
            inspect_schema_state(&db_path).unwrap(),
            SchemaState::RustOwned {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
    }

    #[test]
    fn backup_database_is_byte_identical_and_passes_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.db");
        create_new_database(&src).unwrap();

        let dst = dir.path().join("src.db.bak");
        backup_database(&src, &dst).unwrap();

        assert!(dst.exists());
        let bytes_src = std::fs::read(&src).unwrap();
        let bytes_dst = std::fs::read(&dst).unwrap();
        assert_eq!(bytes_src.len(), bytes_dst.len());

        // backing up again is byte-stable: a second backup reproduces identical bytes
        let dst2 = dir.path().join("src2.db.bak");
        backup_database(&src, &dst2).unwrap();
        assert_eq!(std::fs::read(&dst2).unwrap(), bytes_dst);

        // the backup is RustOwned too (classify, no heal)
        assert_eq!(
            inspect_schema_state(&dst).unwrap(),
            SchemaState::RustOwned {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
    }

    #[test]
    fn compact_database_runs_vacuum() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("compact.db");
        create_new_database(&db_path).unwrap();
        // compact is idempotent and must not error on a freshly-created DB
        compact_database(&db_path).unwrap();
        // still classifies as Rust-owned
        assert!(matches!(
            inspect_schema_state(&db_path).unwrap(),
            SchemaState::RustOwned { .. }
        ));
    }

    #[test]
    fn compact_memory_is_noop() {
        compact_database(Path::new(":memory:")).unwrap();
    }

    #[test]
    fn upgrade_already_current_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("current.db");
        create_new_database(&db_path).unwrap();
        let outcome = upgrade_database(&db_path).unwrap();
        assert_eq!(outcome, UpgradeOutcome::AlreadyCurrent);
    }

    #[test]
    fn upgrade_on_empty_file_creates_fresh_rust_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("empty.db");
        // touch an empty file so the path exists but holds no tables
        std::fs::write(&db_path, b"").unwrap();
        let outcome = upgrade_database(&db_path).unwrap();
        assert_eq!(outcome, UpgradeOutcome::CreatedFresh);
        // now at the current Rust schema
        let probe = Connection::open(&db_path).unwrap();
        let stamp: i64 = probe
            .query_row(
                &format!("SELECT schema_version FROM {RUST_STAMP_TABLE}"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamp, i64::from(RUST_SCHEMA_VERSION));
        assert!(!table_exists(&probe, "alembic_version").unwrap());
    }

    #[test]
    fn upgrade_heals_legacy_marker_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("legacy.db");
        create_new_database(&db_path).unwrap();
        mark_legacy(&db_path);
        assert_eq!(
            inspect_schema_state(&db_path).unwrap(),
            SchemaState::LegacyAlembic
        );

        let outcome = upgrade_database(&db_path).unwrap();
        assert_eq!(outcome, UpgradeOutcome::HealedLegacy);
        assert!(matches!(
            inspect_schema_state(&db_path).unwrap(),
            SchemaState::RustOwned { .. }
        ));
    }

    #[test]
    fn upgrade_unrecognized_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("foreign.db");
        {
            let c = Connection::open(&db_path).unwrap();
            c.execute_batch("CREATE TABLE other (x INTEGER);").unwrap();
        }
        let err = upgrade_database(&db_path).unwrap_err();
        assert!(matches!(err, DbError::UnrecognizedSchema));
    }

    #[test]
    fn backup_corrupt_source_fails_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("garbage.db");
        // not a real `SQLite` file → integrity check fails
        std::fs::write(&src, b"not a database").unwrap();
        let dst = dir.path().join("garbage.db.bak");
        let err = backup_database(&src, &dst).unwrap_err();
        // Either the open or the integrity check rejects it.
        assert!(matches!(
            err,
            DbError::IntegrityCheckFailed(_) | DbError::Sqlite(_)
        ));
    }

    // ── inspect_schema_state + convert_alembic_to_rust_owned (ADR-010 §2) ──

    #[test]
    fn inspect_on_legacy_marker_returns_legacy_alembic() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("legacy.db");
        create_new_database(&db_path).unwrap();
        mark_legacy(&db_path);
        assert_eq!(
            inspect_schema_state(&db_path).unwrap(),
            SchemaState::LegacyAlembic
        );
    }

    #[test]
    fn inspect_on_foreign_file_returns_unrecognized_not_refused() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("foreign.db");
        {
            let c = Connection::open(&db_path).unwrap();
            c.execute_batch("CREATE TABLE other (x INTEGER);").unwrap();
        }
        let state = inspect_schema_state(&db_path).unwrap();
        assert_eq!(state, SchemaState::Unrecognized);
    }

    #[test]
    fn convert_legacy_marker_to_rust_owned_returns_rust_owned() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cutover.db");
        create_new_database(&db_path).unwrap();
        mark_legacy(&db_path);
        assert_eq!(
            inspect_schema_state(&db_path).unwrap(),
            SchemaState::LegacyAlembic
        );

        let state = convert_alembic_to_rust_owned(&db_path).unwrap();
        assert_eq!(
            state,
            SchemaState::RustOwned {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );

        // alembic_version is GONE; the Rust stamp table is stamped; re-open → RustOwned.
        let probe = Connection::open(&db_path).unwrap();
        assert!(!table_exists(&probe, "alembic_version").unwrap());
        let (_db, reopen) = DegenbotDb::open(&db_path).unwrap();
        assert_eq!(reopen, state);
    }

    #[test]
    fn convert_idempotent_on_rust_owned() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("already.db");
        create_new_database(&db_path).unwrap();
        convert_alembic_to_rust_owned(&db_path).unwrap(); // first cutover

        // second cutover is an idempotent no-op → still RustOwned.
        let state = convert_alembic_to_rust_owned(&db_path).unwrap();
        assert_eq!(
            state,
            SchemaState::RustOwned {
                schema_version: RUST_SCHEMA_VERSION,
            }
        );
    }
}
