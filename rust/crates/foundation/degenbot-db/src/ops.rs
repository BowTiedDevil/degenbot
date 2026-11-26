//! `SQLite` **file operations**: create / backup / compact / upgrade.
//!
//! These are write/admin operations — unlike the read handle
//! [`DegenbotDb`][crate::connection::DegenbotDb], whose connections are
//! `PRAGMA query_only=on`, these open their own raw [`Connection`]s, run the
//! full file lifecycle a standalone Rust consumer (`cargo add degenbot-db`)
//! needs, and never set `query_only`. The surface is [`create_new_database`],
//! [`backup_database`], [`compact_database`], [`inspect_schema_state`],
//! [`convert_alembic_to_rust_owned`], and [`upgrade_database`].
//!
//! # Rust-owned from birth
//!
//! [`create_new_database`] stamps the private `_degenbot_db_schema_version`
//! table directly, so a Rust-created DB opens as
//! [`SchemaState::RustOwned`] and never masquerades as a legacy
//! `alembic_version`-marked file it did not create.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

pub use crate::backup::{backup_database, BackupProgress, BackupReport};
pub use crate::error::DbError;
use crate::migrate::{
    apply_fresh_standalone, classify_schema, convert_alembic_to_rust_owned as run_cutover_on_conn,
    stamp_rust_schema_version, SchemaState,
};
use crate::migrations::{apply_rust_migrations, MigrationOutcome};
use crate::pragma::apply_open_pragmas;
use crate::schema::SCHEMA_HEAD;

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

    /// A Rust-owned DB stamped behind the binary was brought forward under the
    /// ADR-052 D2 forward version-lock.
    Migrated {
        /// The stamp before the run.
        from: u32,
        /// The stamp after the run (`== RUST_SCHEMA_VERSION`).
        to: u32,
    },
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
    crate::migrate::ensure_parent_dir(path)?;
    let conn = open_raw(path)?;
    apply_open_pragmas(&conn)?;
    // auto_vacuum must be set before any tables are created; FULL only takes
    // effect on a fresh DB (`SQLite` ignores it otherwise — same as Python).
    conn.execute_batch("PRAGMA auto_vacuum=FULL;")?;
    conn.execute_batch(SCHEMA_HEAD)?;
    conn.execute_batch("VACUUM;")?;
    stamp_rust_schema_version(&conn)?;
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
/// - A Rust-owned DB stamped *behind* the binary → the pending steps are
///   applied under the ADR-052 D2 forward version-lock
///   ([`UpgradeOutcome::Migrated`]) — the same [`apply_rust_migrations`] the
///   open path runs.
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
    crate::migrate::ensure_parent_dir(path)?;
    let conn = open_raw(path)?;
    apply_open_pragmas(&conn)?;

    let state = classify_schema(&conn)?;
    match state {
        SchemaState::FreshStandalone { .. } => {
            // The atomic DDL + stamp (one transaction) — a peer racing this
            // same fresh path either sees the empty file or the complete
            // schema, never the half-built middle that reads as Unrecognized.
            apply_fresh_standalone(&conn)?;
            Ok(UpgradeOutcome::CreatedFresh)
        }
        SchemaState::RustOwned { .. } => {
            // Bring a stale Rust-owned DB forward under the D2 lock — the same
            // registry the open path applies — so `upgrade_database` never
            // reports "already current" for a file stamped behind the binary.
            match apply_rust_migrations(&conn)? {
                MigrationOutcome::AlreadyCurrent { .. } => Ok(UpgradeOutcome::AlreadyCurrent),
                MigrationOutcome::Applied { from, to } => Ok(UpgradeOutcome::Migrated { from, to }),
            }
        }
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
    apply_open_pragmas(&conn)?;
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
    apply_open_pragmas(&conn)?;
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

/// An exclusive advisory `flock` on `path`, held for as long as the value
/// lives (ADR-070 D8's serialization idiom).
///
/// Callers are the file-level admin operations that must not interleave on one
/// DB (heal's build-and-swap, the backup's publish) and the per-market writer
/// that serializes a durable update (`degenbot_aave::AaveMarketWriter`). The
/// lock file is created if absent and never unlinked: unlinking a held `flock`
/// reopens the classic race where a waiter blocked on the unlinked inode and a
/// newcomer re-creating the path hold locks on two different inodes. The kernel
/// releases the lock with the holding process, so a crashed holder cannot wedge
/// the next one.
pub struct AdvisoryLock {
    /// Holding the descriptor is what holds the lock; the lifetime IS the lock.
    _file: std::fs::File,
}

/// Open-or-create the lock file. Never `create_new`: a file surviving its
/// holder is the expected crash case, and the flock — not the file's existence
/// — is the exclusion.
fn open_lock_file(path: &Path) -> Result<std::fs::File, DbError> {
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?)
}

/// Record the holder's pid in the freshly-locked file and wrap the descriptor.
fn held(file: std::fs::File) -> AdvisoryLock {
    use std::io::Write as _;

    let mut file = file;
    let _ = file.set_len(0);
    let _ = writeln!(file, "{}", std::process::id());
    AdvisoryLock { _file: file }
}

impl AdvisoryLock {
    /// Take `path`'s lock, waiting for a current holder to release it.
    pub(crate) fn acquire(path: &Path) -> Result<Self, DbError> {
        let file = open_lock_file(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                tracing::info!(lock = %path.display(), "waiting for another database operation");
                file.lock()?;
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(DbError::Io(error)),
        }
        Ok(held(file))
    }

    /// Take `path`'s lock, or report `None` when another holder has it — the
    /// fail-closed form a second writer of the same resource is refused with.
    ///
    /// # Errors
    ///
    /// [`DbError::Io`] when the lock file cannot be opened or the kernel
    /// refuses the lock for a reason other than a competing holder.
    pub fn try_acquire(path: &Path) -> Result<Option<Self>, DbError> {
        let file = open_lock_file(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(held(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(DbError::Io(error)),
        }
    }
}

/// `path` with `suffix` appended to its file name — a sibling in the same
/// directory, so a `rename` between the two stays atomic.
pub(crate) fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(OsString::new, ToOwned::to_owned);
    name.push(suffix);
    path.with_file_name(name)
}

/// Remove `path`'s `sidecar_suffix` file (`-wal`, `-shm`) when present.
pub(crate) fn remove_sidecar_if_present(path: &Path, sidecar_suffix: &str) {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(sidecar_suffix);
    let _ = std::fs::remove_file(Path::new(&sidecar));
}

/// Rename `src`'s `sidecar_suffix` file to `dst`'s when present, so the two
/// travel together.
pub(crate) fn rename_sidecar_if_present(src: &Path, dst: &Path, sidecar_suffix: &str) {
    let mut src_name = src.as_os_str().to_owned();
    src_name.push(sidecar_suffix);
    if !Path::new(&src_name).exists() {
        return;
    }
    let mut dst_name = dst.as_os_str().to_owned();
    dst_name.push(sidecar_suffix);
    let _ = std::fs::rename(Path::new(&src_name), Path::new(&dst_name));
}

/// Build a `file:` read-only URI for `path`, percent-encoding the characters
/// that are special in a URI or in an SQL string literal. Opening through the
/// URI keeps a read-only handle read-only at the VFS level, so it cannot create
/// or extend `-wal`/`-shm` sidecars.
pub(crate) fn file_ro_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            // Reserved/special in a `file:` URI or an SQL single-quoted string.
            b' ' | b'%' | b'#' | b'?' | b'\'' => {
                uri.push('%');
                let hex = format!("{byte:02X}");
                uri.push_str(&hex);
            }
            _ => uri.push(byte as char),
        }
    }
    uri.push_str("?mode=ro");
    uri
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
    fn advisory_lock_excludes_a_second_holder_until_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        let held = AdvisoryLock::acquire(&path).unwrap();
        // A second acquisition in this process blocks on a separate open file
        // description, so it only completes once the first guard drops.
        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&released);
        let waiter = std::thread::spawn(move || {
            let _guard = AdvisoryLock::acquire(&path).unwrap();
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!released.load(std::sync::atomic::Ordering::SeqCst));
        drop(held);
        waiter.join().unwrap();
        assert!(released.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn try_acquire_reports_the_held_lock_instead_of_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        let held = AdvisoryLock::try_acquire(&path).unwrap();
        assert!(held.is_some(), "an unheld path yields the lock");
        assert!(
            AdvisoryLock::try_acquire(&path).unwrap().is_none(),
            "a held lock is reported, never waited on"
        );
        drop(held);
        assert!(
            AdvisoryLock::try_acquire(&path).unwrap().is_some(),
            "the released lock is acquirable again, from the file the holder left behind"
        );
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
    /// A Rust-owned DB stamped BEHIND the binary is brought forward under the
    /// ADR-052 D2 forward version-lock: `upgrade_database` must not report
    /// `AlreadyCurrent` for a stale Rust-owned file and leave it behind the
    /// binary's schema.
    #[test]
    fn upgrade_database_migrates_a_stale_rust_owned_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("stale.db");
        create_new_database(&db_path).unwrap();

        // Roll the stamp back to 1 — behind RUST_SCHEMA_VERSION.
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            &format!("UPDATE {RUST_STAMP_TABLE} SET schema_version = 1"),
            [],
        )
        .unwrap();
        drop(conn);

        assert_eq!(
            upgrade_database(&db_path).unwrap(),
            UpgradeOutcome::Migrated {
                from: 1,
                to: RUST_SCHEMA_VERSION,
            }
        );

        // A second run is the idempotent no-op.
        assert_eq!(
            upgrade_database(&db_path).unwrap(),
            UpgradeOutcome::AlreadyCurrent
        );

        let probe = Connection::open(&db_path).unwrap();
        let stamp: i64 = probe
            .query_row(
                &format!("SELECT schema_version FROM {RUST_STAMP_TABLE}"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamp, i64::from(RUST_SCHEMA_VERSION));
    }
}
