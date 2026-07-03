//! `PyO3` seam for the `degenbot-db` `SQLite` file operations.
//!
//! Thin `#[pyfunction]` wrappers over [`degenbot_db::ops`]:
//! [`create_new_database`] / [`backup_database`] / [`compact_database`] /
//! [`upgrade_database`]. The core owns the file I/O; these extract the path
//! from Python, release the GIL via `py.detach(...)`, then map [`DbError`] to a
//! Python `ValueError`. No business logic (three-layer architecture, ADR-005).
//!
//! The CLI (`src/degenbot/cli/database.py`'s `backup`/`reset`/`upgrade`/
//! `compact`) delegates here; the inline `create_engine`/`sqlite3.connect`/
//! `command.upgrade` bodies in `src/degenbot/database/operations.py` are
//! retired in favor of these Rust-backed wrappers.

pub mod aave;
pub mod discovery;
pub mod liquidity_updater;
pub mod pool_read;
pub mod snapshot;

use std::path::PathBuf;

use pyo3::create_exception;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::wrap_pyfunction;

pub use aave::PyDatabasePositionQuery;
use degenbot_db::ops::{self, UpgradeOutcome};
pub use liquidity_updater::PyLiquidityUpdateEvent;
pub use pool_read::{
    PyExchangeRow, PyInitializationMapRow, PyLiquidityPoolRow, PyLiquidityPositionRow,
    PyPoolKindRow, PyPoolManagerRow,
};
pub use snapshot::PyDatabaseSnapshot;

// A dedicated exception for the "DB is stamped at a prior Alembic revision"
// rejection (`DbError::AlembicStale`). It subclasses `ValueError` so existing
// broad `except ValueError` handlers keep catching it (and the upgrade shell's
// fall-back below), but gives the CLI a precise type to catch and translate
// into a friendly one-line message — no Python traceback.
//
// The message tells end users to run `degenbot database upgrade` (the
// user-facing migration command), not the developer-oriented
// `alembic upgrade head`. Same pattern as the engine's
// `VerificationMismatchError` / `HookedPoolRejectedError` (typed subclasses of
// a builtin so callers classify by type, not fragile string matching).
create_exception!(
    degenbot_rs,
    DatabaseSchemaStale,
    PyValueError,
    "The database is stamped at a prior Alembic revision; run `degenbot database upgrade`."
);

/// `degenbot_rs.db_create_new_database(path: str) -> None`
///
/// Create a fresh degenbot `SQLite` DB: WAL + head DDL + VACUUM + Alembic stamp.
/// Raises `ValueError` on any failure.
#[pyfunction]
fn db_create_new_database(py: Python<'_>, path: &str) -> PyResult<()> {
    let path = PathBuf::from(path);
    py.detach(|| ops::create_new_database(&path))
        .map_err(|e| db_err_to_py(&e))
}

/// `degenbot_rs.db_backup_database(src: str, dst: str) -> None`
///
/// Online backup with `PRAGMA integrity_check` assertions on both source and
/// destination. Raises `ValueError` on failure.
#[pyfunction]
fn db_backup_database(py: Python<'_>, src: &str, dst: &str) -> PyResult<()> {
    let src = PathBuf::from(src);
    let dst = PathBuf::from(dst);
    py.detach(|| ops::backup_database(&src, &dst))
        .map_err(|e| db_err_to_py(&e))
}

/// `degenbot_rs.db_compact_database(path: str) -> None`
///
/// `VACUUM`. A no-op for `:memory:`. Raises `ValueError` on failure.
#[pyfunction]
fn db_compact_database(py: Python<'_>, path: &str) -> PyResult<()> {
    let path = PathBuf::from(path);
    py.detach(|| ops::compact_database(&path))
        .map_err(|e| db_err_to_py(&e))
}

/// `degenbot_rs.db_upgrade_database(path: str) -> str`
///
/// Ensure the DB is at the Alembic head. Returns `"already_at_head"` if it was
/// current, or `"created_fresh"` if an empty file was brought up to head.
/// Raises `ValueError` for a stale Alembic DB (run `alembic upgrade head` from
/// Python) or an unrecognized schema.
#[pyfunction]
fn db_upgrade_database(py: Python<'_>, path: &str) -> PyResult<String> {
    let path = PathBuf::from(path);
    let outcome = py
        .detach(|| ops::upgrade_database(&path))
        .map_err(|e| db_err_to_py(&e))?;
    Ok(match outcome {
        UpgradeOutcome::AlreadyAtHead => "already_at_head",
        UpgradeOutcome::CreatedFresh => "created_fresh",
    }
    .to_string())
}

/// Map a [`degenbot_db::DbError`] to a Python exception.
///
/// `AlembicStale` becomes a [`DatabaseSchemaStale`] (subclass of `ValueError`)
/// carrying the user-facing "run `degenbot database upgrade`" message — the
/// CLI catches it to print a friendly one-liner instead of a traceback. Every
/// other variant maps to a generic `ValueError` (the degenbot Python layer's
/// convention for database operation failures).
pub(crate) fn db_err_to_py(err: &degenbot_db::DbError) -> PyErr {
    use degenbot_db::DbError;
    match err {
        DbError::AlembicStale { head, expected } => DatabaseSchemaStale::new_err(format!(
            "The database schema is stale (revision {head}; expected {expected}). \
             Run `degenbot database upgrade`."
        )),
        other => PyValueError::new_err(other.to_string()),
    }
}

/// Register the `db` file-op functions on `m` (feature = "db").
///
/// # Errors
///
/// Returns a [`PyErr`] if any `add_function` call fails (e.g. a name
/// collision); propagated unchanged to the `#[pymodule]` caller.
pub fn add_db_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(db_create_new_database, m)?)?;
    m.add_function(wrap_pyfunction!(db_backup_database, m)?)?;
    m.add_function(wrap_pyfunction!(db_compact_database, m)?)?;
    m.add_function(wrap_pyfunction!(db_upgrade_database, m)?)?;
    m.add_function(wrap_pyfunction!(
        liquidity_updater::db_apply_v3_liquidity_updates,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        liquidity_updater::db_apply_v4_liquidity_updates,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(pool_read::db_fetch_pool_row, m)?)?;
    m.add_function(wrap_pyfunction!(pool_read::db_fetch_exchange, m)?)?;
    discovery::add_discovery_module(m)?;
    m.add_class::<liquidity_updater::PyLiquidityUpdateEvent>()?;
    m.add_class::<snapshot::PyDatabaseSnapshot>()?;
    m.add_class::<aave::PyDatabasePositionQuery>()?;
    m.add_class::<pool_read::PyLiquidityPoolRow>()?;
    m.add_class::<pool_read::PyPoolKindRow>()?;
    m.add_class::<pool_read::PyExchangeRow>()?;
    m.add_class::<pool_read::PyLiquidityPositionRow>()?;
    m.add_class::<pool_read::PyInitializationMapRow>()?;
    m.add_class::<pool_read::PyPoolManagerRow>()?;
    Ok(())
}
