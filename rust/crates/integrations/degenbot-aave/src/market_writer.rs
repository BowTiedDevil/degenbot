//! The one Aave market writer: exclusion over a (database, market) pair.
//!
//! A durable Aave write is a market-scoped sequence of transactions — a chunk
//! loop advancing one cursor, or a reset purging one market and re-seeding its
//! substrate. Two of them on the same market interleave into a partially purged
//! market or a cursor advanced under another writer's reads, and SQLite cannot
//! arbitrate: its locks serialize statements, not runs, so every per-chunk
//! commit releases them mid-run.
//!
//! [`AaveMarketWriter`] is that exclusion and the ONLY way to start one of those
//! writes. It is keyed by the canonical database path (the parent directory
//! resolved, so two spellings of one database name one lock file) plus the
//! market id (so two markets of one database proceed independently), and it
//! holds a [`degenbot_db::ops::AdvisoryLock`] for its whole lifetime. The lock
//! file is created if absent and never unlinked: the kernel drops the lock with
//! the holding process, and an unlink would let a newcomer lock a second inode
//! beside the blocked waiter.
//!
//! Acquisition refuses rather than waits, so a second writer learns it is
//! second instead of parking behind a soak. The CLI, the `PyO3` seam, and
//! standalone Rust consumers all acquire this writer, so no caller is the
//! exception.

use std::path::{Path, PathBuf};

use degenbot_db::ops::AdvisoryLock;
use degenbot_db::{AaveMarketPurgeCount, DbError, DegenbotDb};
use degenbot_rpc::provider::AlloyProvider;

use crate::updater::run::{reset_market_to_cold_boot, run_aave_update_on_db};
use crate::updater::{AaveUpdateReport, MarketResetFacts, RunError, RunSpec};

/// One market's exclusive writer, held for the duration of a reset or an update
/// run on it.
///
/// Dropping the writer releases the lock; the lock file stays, so the next
/// writer reuses the inode every earlier holder used.
pub struct AaveMarketWriter {
    database_path: PathBuf,
    market_id: i64,
    /// Holding the descriptor is what holds the lock: the field is never read,
    /// the lifetime IS the exclusion.
    _lock: AdvisoryLock,
}

impl std::fmt::Debug for AaveMarketWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AaveMarketWriter")
            .field("database_path", &self.database_path)
            .field("market_id", &self.market_id)
            .finish_non_exhaustive()
    }
}

/// What one market reset removed, and the facts it read back from the same
/// transaction.
#[derive(Debug, Clone)]
pub struct MarketReset {
    /// The per-relation row counts the purge removed.
    pub removed: Vec<AaveMarketPurgeCount>,
    /// The cold-boot substrate the same transaction left behind.
    pub facts: MarketResetFacts,
}

impl AaveMarketWriter {
    /// Take `market_id`'s writer lock on `database_path`, or refuse when another
    /// writer holds it.
    ///
    /// # Errors
    ///
    /// [`RunError::MarketHeld`] when another holder owns the lock, and
    /// [`RunError::Runtime`] when the database directory cannot be resolved or
    /// the lock file cannot be opened.
    pub fn acquire(database_path: &Path, market_id: i64) -> Result<Self, RunError> {
        let database_path = canonical_database_path(database_path)?;
        let lock_path = market_lock_path(&database_path, market_id);
        let lock = AdvisoryLock::try_acquire(&lock_path)?.ok_or_else(|| RunError::MarketHeld {
            market_id,
            lock_path: lock_path.display().to_string(),
        })?;
        Ok(Self {
            database_path,
            market_id,
            _lock: lock,
        })
    }

    /// The canonical database path this writer guards.
    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// The market row id this writer guards.
    #[must_use]
    pub fn market_id(&self) -> i64 {
        self.market_id
    }

    /// The lock file this writer holds, and every later writer of the pair
    /// reuses.
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        market_lock_path(&self.database_path, self.market_id)
    }

    /// Run one chunked update of this writer's market, opening its own writeable
    /// handle.
    ///
    /// # Errors
    ///
    /// [`RunError::MarketWriterMismatch`] when `spec` names another market, plus
    /// every condition the run itself reports.
    pub fn update(&self, spec: RunSpec) -> Result<AaveUpdateReport, RunError> {
        self.check_market(&spec)?;
        let (db, _state) = DegenbotDb::open_for_writes(&self.database_path)?;
        run_aave_update_on_db(&db, spec)
    }

    /// Run one chunked update through a caller-opened handle, under this
    /// writer's lock.
    ///
    /// The handle is the injection seam the statement-ledger goldens need — the
    /// run must write through THEIR traced connection. The exclusion still comes
    /// from `self`, so a handle-taking caller cannot race another writer any
    /// more than the path-taking one can.
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::update`].
    pub fn update_through(
        &self,
        db: &DegenbotDb,
        spec: RunSpec,
    ) -> Result<AaveUpdateReport, RunError> {
        self.check_market(&spec)?;
        self.check_handle(db)?;
        run_aave_update_on_db(db, spec)
    }

    /// Purge this writer's market and re-seed its cold-boot substrate in ONE
    /// transaction, returning the counts the purge removed and the facts that
    /// transaction left behind.
    ///
    /// # Errors
    ///
    /// Every condition [`reset_market_to_cold_boot`] reports; a failure drops the
    /// transaction, so the database stays byte-identical.
    pub fn reset_to_cold_boot(
        &self,
        pool_address_provider: &str,
        gho_token_address: &str,
        provider: &AlloyProvider,
    ) -> Result<MarketReset, RunError> {
        let (db, _state) = DegenbotDb::open_for_writes(&self.database_path)?;
        // A market that is not there is a typed refusal, never a purge-plan
        // read: the check runs before the transaction opens and before the
        // activation RPC, so an unknown id costs neither.
        if db.fetch_aave_market_row(self.market_id)?.is_none() {
            return Err(RunError::MarketNotFound(self.market_id));
        }
        let mut guard = db.lock();
        let tx = guard.transaction().map_err(DbError::from)?;
        let removed = DegenbotDb::count_aave_market_rows_on_conn(&tx, self.market_id)?;
        let facts = reset_market_to_cold_boot(
            &tx,
            self.market_id,
            pool_address_provider,
            gho_token_address,
            provider,
        )?;
        tx.commit().map_err(DbError::from)?;
        Ok(MarketReset { removed, facts })
    }

    fn check_market(&self, spec: &RunSpec) -> Result<(), RunError> {
        verify_market(self.market_id, spec.market_id)
    }

    /// Refuse a handle onto a database this writer does not guard: its lock
    /// covers one file, and a run through another file's handle would write
    /// outside it.
    fn check_handle(&self, handle: &DegenbotDb) -> Result<(), RunError> {
        let handle_database = handle_database_path(handle);
        if handle_database == self.database_path {
            return Ok(());
        }
        Err(RunError::MarketWriterHandleMismatch {
            writer_database: self.database_path.display().to_string(),
            handle_database: handle_database.display().to_string(),
        })
    }
}

/// A writer guards ONE market: a spec naming another would advance a cursor no
/// lock covers.
fn verify_market(writer_market_id: i64, spec_market_id: i64) -> Result<(), RunError> {
    if writer_market_id == spec_market_id {
        return Ok(());
    }
    Err(RunError::MarketWriterMismatch {
        writer_market_id,
        spec_market_id,
    })
}

/// The lock file one (database, market) pair names: a sibling of the database,
/// so the lock travels with the file it guards and never depends on the cwd.
#[must_use]
pub fn market_lock_path(database_path: &Path, market_id: i64) -> PathBuf {
    let name = database_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut path = database_path.to_path_buf();
    path.set_file_name(format!("{name}.aave-market-{market_id}.lock"));
    path
}

/// Resolve the database's canonical identity, so every spelling of one file keys
/// one lock: the resolved FILE when it exists (symlink aliases and `..`
/// segments included), and the resolved parent directory plus the file name when
/// the file is not there yet.
fn canonical_database_path(database_path: &Path) -> Result<PathBuf, RunError> {
    if let Ok(canonical) = std::fs::canonicalize(database_path) {
        return Ok(canonical);
    }
    let parent = database_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = std::fs::canonicalize(parent)?;
    let name = database_path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} names no database file", database_path.display()),
        )
    })?;
    Ok(directory.join(name))
}

/// The database the open `handle` actually reached, canonicalized so every
/// spelling of one file compares equal.
///
/// The file comes from SQLite's own connection
/// ([`DegenbotDb::main_database_file`]) rather than from a statement, so
/// verifying a handle costs a traced connection no ledger entry. An in-memory
/// `main` has no file identity to match.
fn handle_database_path(handle: &DegenbotDb) -> PathBuf {
    let Some(file) = handle.main_database_file() else {
        return PathBuf::from(":memory:");
    };
    std::fs::canonicalize(&file).unwrap_or(file)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used)]

    use std::fs;
    use std::os::unix::fs::MetadataExt as _;

    use super::*;

    fn database(dir: &Path) -> PathBuf {
        let path = dir.join("degenbot.db");
        degenbot_db::create_new_database(&path).unwrap();
        path
    }

    #[test]
    fn the_lock_file_sits_beside_the_database_and_names_the_market() {
        let dir = tempfile::tempdir().unwrap();
        let db = database(dir.path());
        let lock = market_lock_path(&db, 7);
        assert_eq!(lock.parent(), db.parent());
        assert_eq!(
            lock.file_name().unwrap().to_string_lossy(),
            "degenbot.db.aave-market-7.lock"
        );
    }

    #[test]
    fn a_second_writer_of_one_market_is_refused_until_the_first_drops() {
        let dir = tempfile::tempdir().unwrap();
        let db = database(dir.path());
        let first = AaveMarketWriter::acquire(&db, 7).unwrap();
        assert!(matches!(
            AaveMarketWriter::acquire(&db, 7),
            Err(RunError::MarketHeld { market_id: 7, .. })
        ));
        drop(first);
        AaveMarketWriter::acquire(&db, 7).unwrap();
    }

    #[test]
    fn two_markets_of_one_database_write_independently() {
        let dir = tempfile::tempdir().unwrap();
        let db = database(dir.path());
        let _seven = AaveMarketWriter::acquire(&db, 7).unwrap();
        let _eight = AaveMarketWriter::acquire(&db, 8).unwrap();
        assert_ne!(
            market_lock_path(&db, 7),
            market_lock_path(&db, 8),
            "the market id is part of the key"
        );
    }

    #[test]
    fn two_spellings_of_one_database_name_one_lock() {
        let dir = tempfile::tempdir().unwrap();
        let db = database(dir.path());
        let spelled = dir.path().join(".").join("degenbot.db");
        let absolute_writer = AaveMarketWriter::acquire(&spelled, 7).unwrap();
        assert!(matches!(
            AaveMarketWriter::acquire(&db, 7),
            Err(RunError::MarketHeld { .. })
        ));
        assert_eq!(
            absolute_writer.database_path(),
            db.canonicalize().unwrap(),
            "the writer holds the canonical path"
        );
        assert_eq!(
            absolute_writer.lock_path(),
            market_lock_path(&db.canonicalize().unwrap(), 7)
        );
    }

    #[test]
    fn a_symlink_alias_of_one_database_is_one_owner_one_lock() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real");
        fs::create_dir(&real_dir).unwrap();
        let db = database(&real_dir);
        let alias = dir.path().join("alias.db");
        std::os::unix::fs::symlink(&db, &alias).unwrap();

        let writer = AaveMarketWriter::acquire(&alias, 7).unwrap();
        assert_eq!(
            writer.database_path(),
            db.canonicalize().unwrap(),
            "the alias resolves to the file it names"
        );
        assert!(
            matches!(
                AaveMarketWriter::acquire(&db, 7),
                Err(RunError::MarketHeld { .. })
            ),
            "the real path is the same market the alias holds"
        );
        assert_eq!(
            writer.lock_path(),
            market_lock_path(&db.canonicalize().unwrap(), 7),
            "one lock identity for both spellings"
        );
        drop(writer);
        AaveMarketWriter::acquire(&db, 7).unwrap();
    }

    #[test]
    fn a_not_yet_created_database_keys_its_parent_directory_and_name() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("later.db");
        let alias = dir.path().join(".").join("later.db");
        let held = AaveMarketWriter::acquire(&missing, 7).unwrap();
        assert!(
            matches!(
                AaveMarketWriter::acquire(&alias, 7),
                Err(RunError::MarketHeld { .. })
            ),
            "the fallback identity is the resolved parent plus the file name"
        );
        assert_eq!(
            held.database_path(),
            dir.path().canonicalize().unwrap().join("later.db")
        );
    }

    #[test]
    fn the_lock_file_outlives_its_holder_and_never_recreates() {
        let dir = tempfile::tempdir().unwrap();
        let db = database(dir.path());
        let writer = AaveMarketWriter::acquire(&db, 7).unwrap();
        let lock_path = writer.lock_path();
        let inode_while_held = fs::metadata(&lock_path).unwrap().ino();
        drop(writer);
        let inode_after = fs::metadata(&lock_path).unwrap().ino();
        assert_eq!(
            inode_while_held, inode_after,
            "dropping the writer leaves the lock file in place"
        );
        let next = AaveMarketWriter::acquire(&db, 7).unwrap();
        assert_eq!(
            fs::metadata(next.lock_path()).unwrap().ino(),
            inode_after,
            "the next writer locks the same inode the last one used"
        );
    }

    #[test]
    fn a_deleted_lock_file_is_recreated_with_one_owner() {
        let dir = tempfile::tempdir().unwrap();
        let db = database(dir.path());
        let first = AaveMarketWriter::acquire(&db, 7).unwrap();
        let lock_path = first.lock_path();
        drop(first);
        fs::remove_file(&lock_path).unwrap();
        let second = AaveMarketWriter::acquire(&db, 7).unwrap();
        assert!(lock_path.exists(), "acquire recreates the absent lock file");
        assert!(matches!(
            AaveMarketWriter::acquire(&db, 7),
            Err(RunError::MarketHeld { .. })
        ));
        drop(second);
    }

    #[test]
    fn a_stale_lock_file_from_a_dead_holder_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let db = database(dir.path());
        let lock_path = market_lock_path(&db.canonicalize().unwrap(), 7);
        fs::write(&lock_path, "999999999 dead holder\n").unwrap();
        let writer = AaveMarketWriter::acquire(&db, 7).unwrap();
        assert_eq!(
            fs::read_to_string(&lock_path).unwrap().trim(),
            std::process::id().to_string(),
            "the takeover records the new holder"
        );
        drop(writer);
        assert!(lock_path.exists(), "the file survives its holder");
    }

    #[test]
    fn a_spec_naming_another_market_is_refused() {
        assert!(verify_market(7, 7).is_ok());
        assert!(matches!(
            verify_market(7, 8),
            Err(RunError::MarketWriterMismatch {
                writer_market_id: 7,
                spec_market_id: 8
            })
        ));
    }
}
