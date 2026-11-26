//! The owned read handle: [`DegenbotDb`].
//!
//! A thin `parking_lot::Mutex<rusqlite::Connection>` wrapper (the codebase
//! standard lock discipline — no poisoning, direct guard return, sidesteps
//! `clippy::expect_used`/`unwrap_used`). Construct via [`DegenbotDb::open`]
//! (file) or [`DegenbotDb::open_in_memory`] (tests).
//!
//! # Open PRAGMA sequence
//!
//! Every read connection runs, in order:
//! 1. `PRAGMA busy_timeout=5000;` — per-connection.
//! 2. `PRAGMA synchronous=NORMAL;` — per-connection.
//! 3. `PRAGMA journal_mode=WAL;` — file-persistent, idempotent; matches the
//!    Python open path so production DBs are WAL-on by the time any
//!    Rust read touches them. The switch is retried through fresh-open
//!    contention, which SQLite's busy handler does NOT cover (see
//!    [`crate::pragma`]).
//! 4. the schema gate + the ADR-052 D2 forward version-lock
//!    (`migrate::ensure_schema_at_open`) — an Alembic-stamped DB (head-stamped
//!    OR stale) opens [`SchemaState::LegacyAlembic`], read-only and unhealed:
//!    the heal is explicit, requested by `open_with(path, auto_heal = true, …)`
//!    or by [`crate::ops::upgrade_database`] / [`crate::heal::heal_database`],
//!    which rebuild the legacy DB out-of-place to `RustOwned`. A fresh
//!    standalone file gets the embedded DDL; an unrecognized file refuses. A
//!    Rust-owned DB behind the binary applies its pending steps at open; a DB
//!    ahead of the binary refuses with `DbError::SchemaAhead`; a DB at current
//!    is a no-op.
//! 5. `PRAGMA query_only=on;` — ADR-070 binding #2: once `open()`
//!    returns, every **read** connection is physically incapable of mutating
//!    the DB.
//!
//! Heal is opt-in, one rule for read and write opens alike: every plain
//! `open*` constructor passes `auto_heal = false`, so opening never mutates,
//! and the `LegacyAlembic` disposition reports exactly what was read. The
//! heal's atomic swap renames the source file, so a reader already holding the
//! file mid-heal keeps seeing the OLD inode until its next open (deliberate;
//! the swap is crash-safe by ADR-011).
//!
//! # Why `query_only` follows the schema step (not precedes it)
//!
//! The "before the schema step" ordering covers the three concurrency PRAGMAs
//! (`WAL/busy_timeout/synchronous`) and nothing else; those run first. The
//! `query_only=on` step is set AFTER `ensure_schema_at_open` returns, because
//! the fresh-standalone branch applies the embedded DDL
//! (`CREATE TABLE IF NOT EXISTS ...`) and the heal branch reopens a rebuilt
//! file — writes that `query_only=on` would block.
//!
//! `query_only` is applied to the FINAL connection (the one the heal reopened
//! on the Rust-owned file), so the guarantee holds in the form that
//! matters: **after `open()` returns, every read connection is read-only.**
//! The pinned (`auto_heal = false`) `LegacyAlembic` branch writes nothing,
//! so its practical effect is identical to setting `query_only` first.

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::Connection;

use crate::error::DbError;
use crate::migrate::{ensure_schema_at_open, SchemaState};
use crate::pragma::apply_open_pragmas;

/// The owned read handle wrapping a single pooled [`Connection`].
///
/// Reads take a [`MutexGuard`][parking_lot::MutexGuard] via [`Self::lock`],
/// run a prepared statement, and decode rows.
pub struct DegenbotDb {
    pub(crate) conn: Mutex<Connection>,
}

impl DegenbotDb {
    /// Open a file-backed read handle, run the open PRAGMA sequence + the
    /// schema gate, and return the handle and the schema disposition.
    ///
    /// Opening never mutates the file: the plain `open*` constructors pass
    /// `auto_heal = false`, so a legacy `alembic_version`-marked DB opens as
    /// [`SchemaState::LegacyAlembic`] (nothing written, read-only). The
    /// out-of-place heal — [`crate::heal::heal_database`] under the ADR-011
    /// atomic swap, preserving the original as `*.bak`, landing
    /// [`SchemaState::RustOwned`] — runs only when a caller asks for it:
    /// [`Self::open_with`] with `auto_heal = true`, or
    /// [`crate::ops::upgrade_database`] / [`crate::heal::heal_database`]
    /// directly. An unrecognized file refuses
    /// ([`DbError::UnrecognizedSchema`]), and a fresh standalone file gets the
    /// embedded DDL + the private `_degenbot_db_schema_version` stamp before
    /// `query_only=on` is set.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] if the connection/PRAGMA setup or the heal
    /// fails (a heal failure leaves the original DB untouched — ADR-011), or
    /// [`DbError::UnrecognizedSchema`] if the file is neither a legacy-marker
    /// DB nor a fresh standalone file.
    pub fn open(path: &Path) -> Result<(Self, SchemaState), DbError> {
        Self::open_with(path, false, true)
    }

    /// Open an in-memory read handle (for tests). Runs the full open sequence
    /// → [`ensure_schema`] hits the fresh-standalone path (empty `:memory:`)
    /// and applies the embedded DDL + private stamp.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a connection/PRAGMA/DDL failure.
    pub fn open_in_memory() -> Result<(Self, SchemaState), DbError> {
        Self::open_with(Path::new(":memory:"), false, true)
    }

    /// Open a file-backed **write-capable** handle (the writer substrate).
    /// Same concurrency PRAGMAs ([`crate::pragma::apply_open_pragmas`]) + the
    /// same schema gate as [`Self::open`] — likewise `auto_heal = false`, so
    /// an Alembic-stamped DB opens [`SchemaState::LegacyAlembic`] with no heal
    /// — but `query_only` is **NEVER** set: the connection can
    /// `INSERT`/`UPDATE`.
    ///
    /// This does NOT violate binding #2 ("every **read** connection
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
    /// Returns [`DbError::Sqlite`] if the connection/PRAGMA/DDL setup fails, or
    /// [`DbError::UnrecognizedSchema`] for unrecognized files.
    #[cfg(any(test, feature = "writer"))]
    pub fn open_for_writes(path: &Path) -> Result<(Self, SchemaState), DbError> {
        Self::open_with(path, false, false)
    }

    /// Open an in-memory **write-capable** handle (for writer-substrate tests).
    /// Like [`Self::open_in_memory`] but `query_only` is never set.
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a connection/PRAGMA/DDL failure.
    #[cfg(any(test, feature = "writer"))]
    pub fn open_in_memory_for_writes() -> Result<(Self, SchemaState), DbError> {
        Self::open_with(Path::new(":memory:"), false, false)
    }

    /// Build the handle from [`Self::open_initialized`] and wrap it.
    ///
    /// The explicit-init open: `auto_heal` is the caller's declared intent —
    /// the plain `open*` constructors never heal (opening never mutates).
    ///
    /// # Errors
    ///
    /// Returns [`DbError::Sqlite`] on a connection/PRAGMA/DDL failure, or
    /// [`DbError::UnrecognizedSchema`] if the file is neither a legacy-marker
    /// DB nor a fresh standalone file. With `auto_heal = true`, the heal and its
    /// forward version-lock inside [`ensure_schema_at_open`] additionally
    /// surface [`DbError::Io`], [`DbError::HealVerificationFailed`],
    /// [`DbError::SchemaAhead`], [`DbError::MissingMigrationStep`], and
    /// [`DbError::MigrationStepFailed`].
    pub fn open_with(
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
        // Concurrency PRAGMAs first, before `ensure_schema`. The
        // shared helper retries the fresh-file WAL switch, which SQLite's busy
        // handler does NOT cover — the one fresh-open step a `busy_timeout`
        // cannot absorb (see `crate::pragma`).
        apply_open_pragmas(&conn)?;
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
            SchemaState::Unrecognized => Err(DbError::UnrecognizedSchema),
            other => Ok((conn, other)),
        }
    }

    /// Lock the underlying connection for the duration of a read.
    pub fn lock(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.conn.lock()
    }

    /// The database file this handle actually has open as its `main` schema, or
    /// `None` for an in-memory handle.
    ///
    /// Asked of SQLite's own connection (`sqlite3_db_filename`), not of the path
    /// a caller remembers, so it states the file a write through this handle
    /// would reach. It costs no statement, which is what lets a caller verify a
    /// handle's identity without disturbing a traced connection's statement
    /// ledger.
    #[must_use]
    pub fn main_database_file(&self) -> Option<std::path::PathBuf> {
        let conn = self.conn.lock();
        // SAFETY: the guard owns `conn`'s sqlite3 handle for this whole call,
        // and `sqlite3_db_filename` only reads the connection's registered
        // schema name.
        let name = unsafe { rusqlite::ffi::sqlite3_db_filename(conn.handle(), c"main".as_ptr()) };
        if name.is_null() {
            return None;
        }
        // SAFETY: a non-null return is a NUL-terminated string owned by SQLite
        // and valid while the connection lives (the guard above).
        let file = unsafe { std::ffi::CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned();
        if file.is_empty() {
            return None;
        }
        Some(std::path::PathBuf::from(file))
    }

    /// The handle's guard when no one holds it.
    ///
    /// The non-blocking sibling of [`Self::lock`]: a caller that must prove it
    /// holds no guard at a given moment (a resolution stage parked on an RPC
    /// await) asks here instead of waiting.
    #[must_use]
    pub fn try_lock(&self) -> Option<parking_lot::MutexGuard<'_, Connection>> {
        self.conn.try_lock()
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn main_database_file_states_the_open_file_or_none_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.db");
        let (file_db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
        assert_eq!(
            file_db.main_database_file(),
            Some(path.canonicalize().unwrap()),
            "the handle names the file it opened"
        );

        let (memory_db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        assert_eq!(
            memory_db.main_database_file(),
            None,
            "an in-memory database has no file identity"
        );
    }

    /// Flip a Rust-owned DB to the legacy `alembic_version`-marked shape.
    fn mark_legacy(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "DROP TABLE _degenbot_db_schema_version;\n\
             CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL);\n\
             INSERT INTO alembic_version (version_num) VALUES ('e0aaad8ad486');",
        )
        .unwrap();
    }

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
    fn pinned_open_on_legacy_marker_db_writes_nothing_and_is_read_only() {
        // The `auto_heal=false` posture (the default): the
        // legacy-marker DB opens as LegacyAlembic, read-only, no heal.
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
        assert_eq!(state, SchemaState::LegacyAlembic);
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
    fn auto_heal_on_open_migrates_legacy_marker_db_to_rust_owned() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("heal_legacy.db");
        crate::ops::create_new_database(&db_path).unwrap();
        mark_legacy(&db_path);

        let (db, state) = DegenbotDb::open_with(&db_path, true, true).unwrap();
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
    fn auto_heal_on_open_migrates_a_divergent_legacy_db_to_rust_owned() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("heal_stale.db");
        crate::ops::create_new_database(&db_path).unwrap();
        mark_legacy(&db_path);
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("DROP INDEX ix_erc20_tokens_chain;")
                .unwrap();
        }
        let (_db, state) = DegenbotDb::open_with(&db_path, true, true).unwrap();
        assert!(
            matches!(state, SchemaState::RustOwned { .. }),
            "divergent-legacy auto-heal must land RustOwned, got {state:?}"
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
