//! The owned read handle: [`DegenbotDb`].
//!
//! A thin `parking_lot::Mutex<rusqlite::Connection>` wrapper (the codebase
//! standard lock discipline from `rust/CONTEXT.md` — no poisoning, direct
//! guard return, sidesteps `clippy::expect_used`/`unwrap_used`). Construct via
//! [`DegenbotDb::open`] (file) or [`DegenbotDb::open_in_memory`] (tests).
//!
//! # Open PRAGMA sequence (binding #2/#3)
//!
//! Every read connection runs, in order:
//! 1. `PRAGMA journal_mode=WAL;`  — file-persistent, idempotent; matches the
//!    Python open path (Phase 0, `2KUI3M`) so production DBs are WAL-on by the
//!    time any Rust read touches them.
//! 2. `PRAGMA busy_timeout=5000;` — per-connection.
//! 3. `PRAGMA synchronous=NORMAL;` — per-connection.
//! 4. [`ensure_schema`][crate::migrate::ensure_schema] — reads the
//!    `alembic_version` head; writes NOTHING on an Alembic-stamped DB.
//! 5. `PRAGMA query_only=on;` — **HARD AC** (binding #2): the Rust reader is
//!    physically incapable of mutating an Alembic-stamped production DB during
//!    the hybrid period.
//!
//! # Why `query_only` follows `ensure_schema` (not precedes it)
//!
//! Binding #3 explicitly scopes "before `ensure_schema`" to the three
//! concurrency PRAGMAs (`WAL/busy_timeout/synchronous`); those run first. The
//! `query_only=on` step is set AFTER [`ensure_schema`] returns, because the
//! fresh-standalone branch of [`ensure_schema`] applies the embedded DDL
//! (`CREATE TABLE IF NOT EXISTS ...`) — a write that `query_only=on` blocks.
//! Setting `query_only` after [`ensure_schema`] satisfies the binding #2 hard
//! AC ("every read connection opened by degenbot-db MUST set
//! `query_only=on`") in the form that actually matters: **after `open()`
//! returns, every connection is read-only.** On the `AlembicCurrent` / stale /
//! unrecognized branches `ensure_schema` writes nothing, so the practical
//! effect is identical to setting `query_only` before it; only the
//! fresh-standalone branch differs (it writes DDL with `query_only` still off,
//! then locks down).

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::Connection;

use crate::error::DbError;
use crate::migrate::{ensure_schema, SchemaState};

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
    /// Open a file-backed read handle, run the open PRAGMA sequence +
    /// [`ensure_schema`], and return the handle and the schema disposition.
    ///
    /// On an Alembic-stamped DB the disposition is
    /// [`SchemaState::AlembicCurrent`] and nothing was written. On a stale or
    /// unrecognized DB this returns [`DbError::AlembicStale`] /
    /// [`DbError::UnrecognizedSchema`] (the handle is dropped). On a fresh
    /// standalone file the embedded DDL is applied + the private
    /// `_degenbot_db_schema_version` stamp is written before `query_only=on`
    /// is set.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] if the connection/PRAGMA setup fails,
    /// [`DbError::AlembicStale`] if the DB is stamped at an older Alembic
    /// revision, or [`DbError::UnrecognizedSchema`] if the file is neither an
    /// Alembic DB nor a fresh standalone file.
    pub fn open(path: &Path) -> Result<(Self, SchemaState), DbError> {
        let conn = if path == Path::new(":memory:") {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        let state = Self::finish_open(&conn)?;
        Ok((
            Self {
                conn: Mutex::new(conn),
            },
            state,
        ))
    }

    /// Open an in-memory read handle (for tests). Runs the full open sequence
    /// → [`ensure_schema`] hits the fresh-standalone path (empty `:memory:`)
    /// and applies the embedded DDL + private stamp.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a connection/PRAGMA/DDL failure.
    pub fn open_in_memory() -> Result<(Self, SchemaState), DbError> {
        let conn = Connection::open_in_memory()?;
        let state = Self::finish_open(&conn)?;
        Ok((
            Self {
                conn: Mutex::new(conn),
            },
            state,
        ))
    }

    /// Open a file-backed **write-capable** handle (RQXEKH writer substrate).
    /// Same `PRE_SCHEMA_PRAGMAS` + [`ensure_schema`] sequence as [`Self::open`],
    /// but `query_only` is **NEVER** set — the connection can `INSERT`/`UPDATE`.
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
        let conn = if path == Path::new(":memory:") {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        let state = Self::finish_open_for_writes(&conn)?;
        Ok((
            Self {
                conn: Mutex::new(conn),
            },
            state,
        ))
    }

    /// Open an in-memory **write-capable** handle (for writer-substrate tests).
    /// Like [`Self::open_in_memory`] but `query_only` is never set.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a connection/PRAGMA/DDL failure.
    pub fn open_in_memory_for_writes() -> Result<(Self, SchemaState), DbError> {
        let conn = Connection::open_in_memory()?;
        let state = Self::finish_open_for_writes(&conn)?;
        Ok((
            Self {
                conn: Mutex::new(conn),
            },
            state,
        ))
    }

    /// The shared tail of [`Self::open`] / [`Self::open_in_memory`]: the
    /// PRAGMA sequence + [`ensure_schema`] + the post-schema `query_only=on`.
    /// Refuses (returns [`DbError`]) on stale/unrecognized schemas.
    fn finish_open(conn: &Connection) -> Result<SchemaState, DbError> {
        Self::finish_open_inner(conn, /* set_query_only */ true)
    }

    /// The shared tail of [`Self::open_for_writes`] /
    /// [`Self::open_in_memory_for_writes`]: the PRAGMA sequence +
    /// [`ensure_schema`] but `query_only` is NOT set (write-capable).
    fn finish_open_for_writes(conn: &Connection) -> Result<SchemaState, DbError> {
        Self::finish_open_inner(conn, /* set_query_only */ false)
    }

    /// The full shared open tail: PRAGMAs + [`ensure_schema`] + optional
    /// `query_only=on` + the stale/unrecognized refuse check.
    fn finish_open_inner(conn: &Connection, set_query_only: bool) -> Result<SchemaState, DbError> {
        // Concurrency PRAGMAs first (binding #3: before ensure_schema).
        conn.execute_batch(PRE_SCHEMA_PRAGMAS)?;

        // ensure_schema next. On the FreshStandalone branch it applies the
        // embedded DDL (a write) — so query_only must NOT be set yet. On the
        // Alembic branches it writes nothing.
        let state = ensure_schema(conn)?;

        // Read handles lock down here (binding #2). Writer handles skip this so
        // the upsert substrate can INSERT/UPDATE.
        if set_query_only {
            conn.pragma_update(None, "query_only", "on")?;
        }

        match state {
            SchemaState::AlembicStale { head, expected } => {
                Err(DbError::AlembicStale { head, expected })
            }
            SchemaState::Unrecognized => Err(DbError::UnrecognizedSchema),
            other => Ok(other),
        }
    }

    /// Lock the underlying connection for the duration of a read.
    pub fn lock(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.conn.lock()
    }
}

#[cfg(test)]
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
    fn open_on_alembic_stamped_db_writes_nothing_and_is_read_only() {
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
        let (db, state) = DegenbotDb::open(&db_path).unwrap();
        assert_eq!(state, SchemaState::AlembicCurrent);
        let conn = db.lock();
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
    fn open_on_stale_alembic_db_refuses() {
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
        let result = DegenbotDb::open(&db_path);
        assert!(
            matches!(result, Err(DbError::AlembicStale { .. })),
            "expected AlembicStale refusal"
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
}
