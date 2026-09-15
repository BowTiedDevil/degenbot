//! The owned read handle: [`DegenbotDb`].
//!
//! A thin `parking_lot::Mutex<rusqlite::Connection>` wrapper (the codebase
//! standard lock discipline — no poisoning, direct guard return, sidesteps
//! `clippy::expect_used`/`unwrap_used`). Construct via [`DegenbotDb::open`]
//! (file) or [`DegenbotDb::open_in_memory`] (tests).
//!
//! # Open PRAGMA sequence (binding #2/#3)
//!
//! Every read connection runs, in order:
//! 1. `PRAGMA journal_mode=WAL;`  — file-persistent, idempotent; matches the
//!    Python open path (Phase 0, `2KUI3M`) so production DBs are WAL-on by the
//!    time any Rust read touches them.
//! 2. `PRAGMA busy_timeout=5000;` — per-connection.
//! 3. `PRAGMA synchronous=NORMAL;` — per-connection.
//! 4. the schema gate + ADR-052 D1 heal-at-open (`migrate::ensure_schema_at_open`)
//!    — an Alembic-stamped DB (head-stamped OR stale) is healed out-of-place to
//!    `RustOwned` unless `DEGENBOT_DB_AUTO_HEAL=0` pins the pre-D1 posture; a
//!    fresh standalone file gets the embedded DDL; an unrecognized file refuses.
//! 5. `PRAGMA query_only=on;` — **HARD AC** (binding #2): once `open()`
//!    returns, every **read** connection is physically incapable of mutating
//!    the DB.
//!
//! Heal-at-open runs at BOTH read and write opens — one rule for all opens: a
//! stale read that did not heal would be a lie about the schema. The heal's
//! atomic swap renames the source file, so a reader already holding the file
//! mid-heal keeps seeing the OLD inode until its next open (deliberate; the
//! swap is crash-safe by ADR-011).
//!
//! # Why `query_only` follows the schema step (not precedes it)
//!
//! Binding #3 explicitly scopes "before `ensure_schema`" to the three
//! concurrency PRAGMAs (`WAL/busy_timeout/synchronous`); those run first. The
//! `query_only=on` step is set AFTER `ensure_schema_at_open` returns, because
//! the fresh-standalone branch applies the embedded DDL
//! (`CREATE TABLE IF NOT EXISTS ...`) and the heal branch reopens a rebuilt
//! file — writes that `query_only=on` would block.
//!
//! `query_only` is applied to the FINAL connection (the one the heal reopened
//! on the Rust-owned file), so binding #2's hard AC holds in the form that
//! matters: **after `open()` returns, every read connection is read-only.**
//! The pinned (`DEGENBOT_DB_AUTO_HEAL=0`) `AlembicCurrent` branch writes nothing,
//! so its practical effect is identical to setting `query_only` first.

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::Connection;

use crate::error::DbError;
use crate::migrate::{auto_heal_enabled, ensure_schema_at_open, SchemaState};

/// The owned read handle wrapping a single pooled [`Connection`].
///
/// Reads take a [`MutexGuard`][parking_lot::MutexGuard] via [`Self::lock`],
/// run a prepared statement, and decode rows. The `PyO3` wrapper (sibling task,
/// slice 14c) will hold an `Arc<DegenbotDb>` on `PyBotIo.db` in place of
/// today's `Option<Py<PyAny>>`.
pub struct DegenbotDb {
    pub(crate) conn: Mutex<Connection>,
}

/// The per-connection PRAGMAs the open path always sets (binding #3): the
/// three concurrency PRAGMAs that must run BEFORE [`ensure_schema`] (WAL is
/// file-persistent; `busy_timeout`/`synchronous` are per-connection).
const PRE_SCHEMA_PRAGMAS: &str = "PRAGMA journal_mode=WAL;\n\
                                  PRAGMA busy_timeout=5000;\n\
                                  PRAGMA synchronous=NORMAL;";

impl DegenbotDb {
    /// Open a file-backed read handle, run the open PRAGMA sequence + the
    /// ADR-052 D1 heal-at-open, and return the handle and the schema
    /// disposition.
    ///
    /// An Alembic-stamped DB (head-stamped OR stale) is healed out-of-place at
    /// open ([`crate::heal::heal_database`] under the ADR-011 atomic swap,
    /// preserving the original as `*.bak`) and the disposition is
    /// [`SchemaState::RustOwned`]. With `DEGENBOT_DB_AUTO_HEAL=0` the pre-D1
    /// posture is restored: head → [`SchemaState::AlembicCurrent`] (nothing
    /// written), stale → [`DbError::AlembicStale`]. An unrecognized file still
    /// refuses ([`DbError::UnrecognizedSchema`]), and a fresh standalone file
    /// gets the embedded DDL + the private `_degenbot_db_schema_version` stamp
    /// before `query_only=on` is set.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] if the connection/PRAGMA setup or the heal
    /// fails (a heal failure leaves the original DB untouched — ADR-011),
    /// [`DbError::AlembicStale`] for a stale DB under the killswitch, or
    /// [`DbError::UnrecognizedSchema`] if the file is neither an Alembic DB nor
    /// a fresh standalone file.
    pub fn open(path: &Path) -> Result<(Self, SchemaState), DbError> {
        Self::open_with(path, auto_heal_enabled(), true)
    }

    /// Open an in-memory read handle (for tests). Runs the full open sequence
    /// → [`ensure_schema`] hits the fresh-standalone path (empty `:memory:`)
    /// and applies the embedded DDL + private stamp.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a connection/PRAGMA/DDL failure.
    pub fn open_in_memory() -> Result<(Self, SchemaState), DbError> {
        Self::open_with(Path::new(":memory:"), auto_heal_enabled(), true)
    }

    /// Open a file-backed **write-capable** handle (RQXEKH writer substrate).
    /// Same `PRE_SCHEMA_PRAGMAS` + ADR-052 D1 heal-at-open as [`Self::open`]
    /// (an Alembic-stamped DB heals to [`SchemaState::RustOwned`]; the
    /// `DEGENBOT_DB_AUTO_HEAL=0` killswitch restores the pre-D1 posture), but
    /// `query_only` is **NEVER** set — the connection can `INSERT`/`UPDATE`.
    ///
    /// This does NOT violate SLHSM4 binding #2 ("every **read** connection
    /// opened by degenbot-db MUST set `query_only=on`"): the *read* constructors
    /// ([`Self::open`] / [`Self::open_in_memory`]) stay read-only; this is the
    /// explicit opt-in writer path used by the Aave writer substrate
    /// ([`crate::write`]) + the `PyO3` seam's write-through path. Called on a
    /// read handle, the writer methods ([`crate::write::DegenbotDb`]
    /// `get_or_create_*` / `process_*`) fail at the `SQLite` layer
    /// (`attempt to write a readonly database`).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] if the connection/PRAGMA/DDL setup fails,
    /// [`DbError::AlembicStale`] / [`DbError::UnrecognizedSchema`] for
    /// unrecognized files.
    pub fn open_for_writes(path: &Path) -> Result<(Self, SchemaState), DbError> {
        Self::open_with(path, auto_heal_enabled(), false)
    }

    /// Open an in-memory **write-capable** handle (for writer-substrate tests).
    /// Like [`Self::open_in_memory`] but `query_only` is never set.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a connection/PRAGMA/DDL failure.
    pub fn open_in_memory_for_writes() -> Result<(Self, SchemaState), DbError> {
        Self::open_with(Path::new(":memory:"), auto_heal_enabled(), false)
    }

    /// Build the handle from [`Self::open_initialized`] and wrap it.
    fn open_with(
        path: &Path,
        auto_heal: bool,
        set_query_only: bool,
    ) -> Result<(Self, SchemaState), DbError> {
        let (conn, state) = Self::open_initialized(path, auto_heal, set_query_only)?;
        Ok((
            Self {
                conn: Mutex::new(conn),
            },
            state,
        ))
    }

    /// A raw connection to `path` (`:memory:` supported) with the three
    /// concurrency PRAGMAs already applied — the factory
    /// [`ensure_schema_at_open`] calls for the initial open AND the post-heal
    /// reopen.
    fn open_primed(path: &Path) -> Result<Connection, DbError> {
        let conn = if path == Path::new(":memory:") {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        // Concurrency PRAGMAs first (binding #3: before ensure_schema).
        conn.execute_batch(PRE_SCHEMA_PRAGMAS)?;
        Ok(conn)
    }

    /// The full shared open tail: PRAGMAs + the ADR-052 D1 heal-at-open
    /// ([`ensure_schema_at_open`]) + optional `query_only=on` + the
    /// stale/unrecognized refuse check.
    ///
    /// `query_only` is applied AFTER the schema step because the
    /// `FreshStandalone` branch applies the embedded DDL (a write) and the
    /// healed branch reopens a fresh file. Read handles lock down here
    /// (binding #2); writer handles skip it so the upsert substrate can
    /// INSERT/UPDATE.
    fn open_initialized(
        path: &Path,
        auto_heal: bool,
        set_query_only: bool,
    ) -> Result<(Connection, SchemaState), DbError> {
        let (conn, state) = ensure_schema_at_open(path, auto_heal, Self::open_primed)?;

        if set_query_only {
            conn.pragma_update(None, "query_only", "on")?;
        }

        match state {
            SchemaState::AlembicStale { head, expected } => {
                Err(DbError::AlembicStale { head, expected })
            }
            SchemaState::Unrecognized => Err(DbError::UnrecognizedSchema),
            other => Ok((conn, other)),
        }
    }

    /// Lock the underlying connection for the duration of a read.
    pub fn lock(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.conn.lock()
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_sets_pragmas_and_query_only() {
        let (db, state) = DegenbotDb::open_in_memory().unwrap();
        assert!(matches!(state, SchemaState::FreshStandalone { .. }));
        let conn = db.lock();
        // query_only must be ON — a write must fail.
        let write_attempt: rusqlite::Result<usize> = conn.execute("CREATE TABLE x (a INT)", []);
        assert!(
            write_attempt.is_err(),
            "query_only=on should block writes, got {write_attempt:?}",
        );
        // the three concurrency PRAGMAs:
        let journal: String = conn
            .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
            .unwrap();
        // in-memory DBs report "memory" (SQLite does not support WAL for :memory:);
        // file-backed DBs report "wal". Accept either — the concurrency PRAGMA
        // sequence itself is what matters; WAL is enforced on the file path.
        assert!(
            journal.eq_ignore_ascii_case("wal") || journal.eq_ignore_ascii_case("memory"),
            "journal_mode was {journal:?}",
        );
        let busy: i64 = conn
            .query_row("PRAGMA busy_timeout;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(busy, 5000);
        let sync: i64 = conn
            .query_row("PRAGMA synchronous;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1); // NORMAL == 1
    }

    #[test]
    fn pinned_open_on_alembic_stamped_db_writes_nothing_and_is_read_only() {
        // The `DEGENBOT_DB_AUTO_HEAL=0` posture (`auto_heal=false`): the
        // head-stamped DB opens as AlembicCurrent, read-only, no heal.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("stamped.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
                 INSERT INTO alembic_version (version_num) VALUES ('2606a6c7f5ee');",
            )
            .unwrap();
        }
        let (conn, state) = DegenbotDb::open_initialized(&db_path, false, true).unwrap();
        assert_eq!(state, SchemaState::AlembicCurrent);
        // no degenbot tables were created
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pools'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        // query_only still on
        assert!(conn.execute("CREATE TABLE y (a INT)", []).is_err());
    }

    #[test]
    fn pinned_open_on_stale_alembic_db_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("stale.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
                 INSERT INTO alembic_version (version_num) VALUES ('000000000000');",
            )
            .unwrap();
        }
        let result = DegenbotDb::open_initialized(&db_path, false, true);
        assert!(
            matches!(result, Err(DbError::AlembicStale { .. })),
            "expected AlembicStale refusal under the killswitch"
        );
    }

    #[test]
    fn open_on_unrecognized_db_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("foreign.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("CREATE TABLE other (x INTEGER);")
                .unwrap();
        }
        let result = DegenbotDb::open(&db_path);
        assert!(
            matches!(result, Err(DbError::UnrecognizedSchema)),
            "expected UnrecognizedSchema refusal"
        );
    }

    #[test]
    fn auto_heal_on_open_migrates_head_stamped_db_to_rust_owned() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("heal_head.db");
        crate::ops::create_new_database(&db_path).unwrap();

        let (db, state) = DegenbotDb::open(&db_path).unwrap();
        assert!(
            matches!(state, SchemaState::RustOwned { .. }),
            "auto-heal must land RustOwned, got {state:?}"
        );
        let conn = db.lock();
        // The healed file is still read-locked down.
        assert!(
            conn.execute("CREATE TABLE y (a INT)", []).is_err(),
            "query_only must be on after a healed open"
        );
        drop(conn);

        let mut name = db_path.file_name().unwrap().to_owned();
        name.push(".bak");
        assert!(
            db_path.with_file_name(name).exists(),
            "the pre-heal DB must persist as `.bak`"
        );
    }

    #[test]
    fn auto_heal_on_open_migrates_stale_db_to_rust_owned() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("heal_stale.db");
        crate::ops::create_new_database(&db_path).unwrap();
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "DROP INDEX ix_erc20_tokens_chain;\n\
                 UPDATE alembic_version SET version_num='e0aaad8ad486';",
            )
            .unwrap();
        }
        let (_db, state) = DegenbotDb::open(&db_path).unwrap();
        assert!(
            matches!(state, SchemaState::RustOwned { .. }),
            "stale-rev auto-heal must land RustOwned, got {state:?}"
        );
    }

    #[test]
    fn fresh_standalone_open_is_not_healed() {
        // A non-Alembic fresh file passes through with no heal and no `.bak`.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("fresh.db");
        let (_db, state) = DegenbotDb::open(&db_path).unwrap();
        assert!(matches!(state, SchemaState::FreshStandalone { .. }));
        let mut name = db_path.file_name().unwrap().to_owned();
        name.push(".bak");
        assert!(!db_path.with_file_name(name).exists());
    }
}
