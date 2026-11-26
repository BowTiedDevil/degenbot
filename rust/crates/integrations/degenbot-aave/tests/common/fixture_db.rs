//! A file-backed fixture database + the writer key it names.
//!
//! The updater's exclusion is a lock file beside the database, so every fixture a
//! run writes is a real file: an in-memory handle has no path to key a lock on, and
//! no second writer it needs excluding from. The temp directory travels with the
//! handle, so the database outlives the fixture value.
//!
//! Declared per suite with `#[path = "common/fixture_db.rs"] mod fixture_db;` rather
//! than from `common/mod.rs`, because a suite that never seeds a run fixture (the
//! projection-equivalence replay) must not compile it.

#![expect(clippy::expect_used)]
// reason: a fixture builder's prerequisites — a temp dir, a schema open — are
// harness facts, never test expectations.

use std::ops::Deref;
use std::path::{Path, PathBuf};

use degenbot_db::DegenbotDb;
use tempfile::TempDir;

/// A file-backed fixture database plus the writer key it names.
pub struct FixtureDb {
    db: DegenbotDb,
    path: PathBuf,
    _dir: TempDir,
}

impl FixtureDb {
    /// A fresh empty database named `name` in its own temp directory.
    ///
    /// # Panics
    ///
    /// When the temp directory or the database cannot be created — a harness
    /// prerequisite, never a test expectation.
    #[must_use]
    pub fn create(name: &str) -> Self {
        let dir = TempDir::new().expect("the fixture temp dir");
        let path = dir.path().join(name);
        let (db, _state) = DegenbotDb::open_for_writes(&path).expect("the fixture database opens");
        Self {
            db,
            path,
            _dir: dir,
        }
    }

    /// The database's own path — the key a market writer takes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Deref for FixtureDb {
    type Target = DegenbotDb;

    fn deref(&self) -> &DegenbotDb {
        &self.db
    }
}
