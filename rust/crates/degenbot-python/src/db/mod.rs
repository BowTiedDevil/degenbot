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
use pyo3::types::PyDict;
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

/// `degenbot_rs.db_inspect_schema_state(database_path: str) -> str`
///
/// The read-only dry-run companion to `db_convert_alembic_to_rust_owned`:
/// reports the schema state WITHOUT writing. Never refuses (reports even
/// stale / unrecognized states). Returns one of `"alembic_current"`,
/// `"alembic_stale"`, `"fresh_standalone"`, `"rust_owned"`, `"unrecognized"`.
/// Raises `ValueError` only on a genuine `SQLite` open/query failure.
#[pyfunction]
fn db_inspect_schema_state(py: Python<'_>, database_path: &str) -> PyResult<String> {
    let path = PathBuf::from(database_path);
    let state = py
        .detach(|| ops::inspect_schema_state(&path))
        .map_err(|e| db_err_to_py(&e))?;
    Ok(schema_state_label(&state).to_string())
}

/// `degenbot_rs.db_convert_alembic_to_rust_owned(database_path: str) -> str`
///
/// The opt-in one-way cutover (ADR-010): flip an Alembic-stamped DB into Rust
/// ownership — `DROP`s `alembic_version`, stamps `_degenbot_db_schema_version`.
/// Returns `"converted"` (was `AlembicCurrent`) or `"already_rust_owned"` (was
/// already Rust-owned → idempotent no-op). Raises `DatabaseSchemaStale` for a
/// stale Alembic DB (run `degenbot database upgrade` first) or `ValueError`
/// for an unrecognized (foreign) file.
#[pyfunction]
fn db_convert_alembic_to_rust_owned(py: Python<'_>, database_path: &str) -> PyResult<String> {
    let path = PathBuf::from(database_path);
    // Inspect the PRE-cutover state first (to distinguish a real cutover from
    // an idempotent no-op on an already-Rust-owned DB). classify_schema reads
    // without writing.
    let pre = py
        .detach(|| ops::inspect_schema_state(&path))
        .map_err(|e| db_err_to_py(&e))?;
    // Run the cutover. Refuses AlembicStale / Unrecognized via DbError.
    py.detach(|| ops::convert_alembic_to_rust_owned(&path))
        .map_err(|e| db_err_to_py(&e))?;
    Ok(match pre {
        degenbot_db::SchemaState::RustOwned { .. } => "already_rust_owned",
        // FreshStandalone would also re-stamp, but the cutover op is meant for
        // AlembicCurrent DBs; treat any non-RustOwned pre-state as a real
        // cutover (the substrate stamps regardless).
        _ => "converted",
    }
    .to_string())
}

/// `degenbot_rs.db_heal_database(database_path: str) -> dict`
///
/// Out-of-place dump-and-restore "heal" (ADR-011): builds a fresh DB at the
/// Rust head schema, copies all user rows from the old DB (preserving PKs +
/// FK integrity, in FK-dependency order), stamps `RustOwned` directly (never
/// runs Alembic code), then atomically swaps it into place (old DB preserved
/// as `*.bak` for full recoverability). Never mutates the old DB in place —
/// a read-only open of the old DB feeds the copy, so the old file is left
/// byte-identical until the final `rename`.
///
/// Returns a dict:
///   `{"old_state": str, "rows_copied": dict[str, int], "bak_path": str,
///     "new_state": str, "warnings": list[str]}``
/// - `old_state` / `new_state`: `schema_state_label` of the `HealReport` fields.
/// - No-op if old is already `rust_owned` (returns `old_state == new_state ==
///   "rust_owned"`, empty `rows_copied`, `bak_path == database_path`).
/// - Refuses `Unrecognized` (foreign file) + any I/O / copy / verification
///   failure via `db_err_to_py` (`ValueError`).
///
/// The GIL is released across the entire read-copy-swap (the
/// `py.detach(|| ops::heal_database(&path))` call) so a long copy doesn't
/// freeze Python threads.
#[pyfunction]
fn db_heal_database(py: Python<'_>, database_path: &str) -> PyResult<Py<PyDict>> {
    let path = PathBuf::from(database_path);
    let report = py
        .detach(|| ops::heal_database(&path))
        .map_err(|e| db_err_to_py(&e))?;

    let dict = PyDict::new(py);
    dict.set_item("old_state", schema_state_label(&report.old_state))?;
    dict.set_item("new_state", schema_state_label(&report.new_state))?;
    dict.set_item("bak_path", report.bak_path.to_string_lossy().to_string())?;

    // `rows_copied` sub-dict: sorted keys for deterministic iteration (a
    // HashMap's order is random; pin a stable order for snapshot tests).
    let rows_dict = PyDict::new(py);
    let mut rows: Vec<(String, u64)> = report.rows_copied.into_iter().collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in rows {
        rows_dict.set_item(k, v)?;
    }
    dict.set_item("rows_copied", rows_dict)?;

    dict.set_item("warnings", report.warnings)?;
    Ok(dict.unbind())
}

/// Map a [`degenbot_db::SchemaState`] to its Python label string (the
/// `db_inspect_schema_state` return value).
fn schema_state_label(state: &degenbot_db::SchemaState) -> &'static str {
    use degenbot_db::SchemaState;
    match state {
        SchemaState::AlembicCurrent => "alembic_current",
        SchemaState::AlembicStale { .. } => "alembic_stale",
        SchemaState::FreshStandalone { .. } => "fresh_standalone",
        SchemaState::RustOwned { .. } => "rust_owned",
        SchemaState::Unrecognized => "unrecognized",
    }
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
    m.add_function(wrap_pyfunction!(db_inspect_schema_state, m)?)?;
    m.add_function(wrap_pyfunction!(db_convert_alembic_to_rust_owned, m)?)?;
    m.add_function(wrap_pyfunction!(db_heal_database, m)?)?;
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
    m.add_function(wrap_pyfunction!(pool_read::db_fetch_exchange_by_name, m)?)?;
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
