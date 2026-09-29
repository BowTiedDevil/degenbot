//! `PyO3` seam for the `degenbot-db` `SQLite` file operations.
//!
//! Thin `#[pyfunction]` wrappers over [`degenbot_db::ops`]:
//! [`backup_database`] / [`compact_database`] / [`upgrade_database`]. The core
//! owns the file I/O; these extract the path
//! from Python, release the GIL via `py.detach(...)`, then map [`DbError`] to a
//! Python `ValueError`. No business logic (three-layer architecture, ADR-005).
//!
//! The Rust CLI and the stable `degenbot.db` mirror delegate here; Python
//! consumers use the typed wrappers rather than opening SQLite directly.

pub mod aave;
#[cfg(feature = "aave-updater")]
pub mod aave_analysis;
pub mod discovery;
pub mod liquidity_updater;
pub mod pool_read;
pub mod read_seams;
pub mod snapshot;

use std::path::PathBuf;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

pub use aave::PyDatabasePositionQuery;
use degenbot_db::ops::{self, UpgradeOutcome};
use degenbot_db::schema::RUST_SCHEMA_VERSION;
pub use liquidity_updater::PyLiquidityUpdateEvent;
pub use pool_read::{PyExchangeRow, PyLiquidityPoolRow, PyPoolManagerRow};
pub use snapshot::PyDatabaseSnapshot;

/// `degenbot._ffi.db.db_backup_database(src: str, dst: str) -> None`
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

/// `degenbot._ffi.db.db_compact_database(path: str) -> None`
///
/// `VACUUM`. A no-op for `:memory:`. Raises `ValueError` on failure.
#[pyfunction]
fn db_compact_database(py: Python<'_>, path: &str) -> PyResult<()> {
    let path = PathBuf::from(path);
    py.detach(|| ops::compact_database(&path))
        .map_err(|e| db_err_to_py(&e))
}

/// `degenbot._ffi.db.db_schema_version() -> int`
///
/// The Rust core's schema version ([`degenbot_db::schema::RUST_SCHEMA_VERSION`])
/// — the value stamped into `_degenbot_db_schema_version` on create / heal /
/// migrate. Exposed so the Python driver (and its parity tests) pin to the
/// same constant the core writes instead of hardcoding it.
#[pyfunction]
fn db_schema_version() -> u32 {
    RUST_SCHEMA_VERSION
}

/// `degenbot._ffi.db.db_upgrade_database(path: str) -> str`
///
/// Ensure the DB is at the current Rust schema. Returns `"already_current"` if
/// it was already there, `"created_fresh"` if an empty file was brought up, or
/// `"healed_legacy"` if a legacy `alembic_version`-marked DB was healed
/// out-of-place. Raises `ValueError` for an unrecognized schema.
#[pyfunction]
fn db_upgrade_database(py: Python<'_>, path: &str) -> PyResult<String> {
    let path = PathBuf::from(path);
    let outcome = py
        .detach(|| ops::upgrade_database(&path))
        .map_err(|e| db_err_to_py(&e))?;
    Ok(match outcome {
        UpgradeOutcome::AlreadyCurrent => "already_current",
        UpgradeOutcome::CreatedFresh => "created_fresh",
        UpgradeOutcome::HealedLegacy => "healed_legacy",
    }
    .to_string())
}

/// `degenbot._ffi.db.db_inspect_schema_state(database_path: str) -> str`
///
/// Reports the schema state WITHOUT writing. Never refuses (reports even
/// legacy / unrecognized states). Returns one of `"legacy_alembic"`,
/// `"fresh_standalone"`, `"rust_owned"`, `"unrecognized"`.
/// Raises `ValueError` only on a genuine `SQLite` open/query failure.
#[pyfunction]
fn db_inspect_schema_state(py: Python<'_>, database_path: &str) -> PyResult<String> {
    let path = PathBuf::from(database_path);
    let state = py
        .detach(|| ops::inspect_schema_state(&path))
        .map_err(|e| db_err_to_py(&e))?;
    Ok(schema_state_label(&state).to_string())
}

/// `degenbot._ffi.db.db_heal_database(database_path: str) -> dict`
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
        SchemaState::LegacyAlembic => "legacy_alembic",
        SchemaState::FreshStandalone { .. } => "fresh_standalone",
        SchemaState::RustOwned { .. } => "rust_owned",
        SchemaState::Unrecognized => "unrecognized",
    }
}

/// Map a [`degenbot_bot::bot_core::tick_assembly::TickMapAssemblyError`] to a
/// Python exception.
///
/// - `Db` variant → delegates to [`db_err_to_py`] (a `ValueError`).
/// - `Chain` variant → `RuntimeError` (Decision 8 (A) loud-failure posture —
///   RPC failures surface as a typed exception, not swallowed into a silent
///   degrade-to-sparse path).
pub(crate) fn assembly_err_to_py(
    err: &degenbot_bot::bot_core::tick_assembly::TickMapAssemblyError,
) -> PyErr {
    use degenbot_bot::bot_core::tick_assembly::TickMapAssemblyError;
    match err {
        TickMapAssemblyError::Db(e) => db_err_to_py(e),
        TickMapAssemblyError::Chain(e) => pyo3::exceptions::PyRuntimeError::new_err(e.to_string()),
        TickMapAssemblyError::InconsistentTickMap { .. } => {
            pyo3::exceptions::PyValueError::new_err(err.to_string())
        }
    }
}

/// Map a [`degenbot_db::DbError`] to a Python exception.
///
/// Every variant maps to a generic `ValueError` (the degenbot Python layer's
/// convention for database operation failures).
pub(crate) fn db_err_to_py(err: &degenbot_db::DbError) -> PyErr {
    PyValueError::new_err(err.to_string())
}

/// The `degenbot._ffi.db` Python submodule (declarative `#[pymodule]`),
/// registering all ~45 `db_*` pyfunctions + ~11 pyclasses. The `db_` prefix
/// is retained on the submodule names (unlike the math submodules which
/// dropped their prefix) because (a) the blast radius is much larger (5 Rust
/// files, ~45 fns) and (b) `db_` functions as a functional namespace marker
/// is clearer than bare `backup_database` on a `db` submodule.
///
/// The companion `src/degenbot/database/_ffi.py` re-exports these as the
/// stable import path, decoupling Python consumers from `degenbot._ffi`.
/// The parent module registers the submodule itself and its `sys.modules`
/// entry.
#[pymodule(submodule)]
#[pyo3(module = "degenbot._ffi")]
pub mod db {
    // Core file ops (this file).
    #[pymodule_export]
    use super::{
        db_backup_database, db_compact_database, db_heal_database, db_inspect_schema_state,
        db_schema_version, db_upgrade_database,
    };

    // Liquidity-updater write path.
    #[pymodule_export]
    use super::liquidity_updater::{
        db_apply_v3_liquidity_updates, db_apply_v4_liquidity_updates, PyLiquidityUpdateEvent,
    };

    // Pool/exchange read rows.
    #[pymodule_export]
    use super::aave::PyDatabasePositionQuery;
    #[pymodule_export]
    use super::pool_read::{
        db_fetch_exchange, db_fetch_exchange_by_name, db_fetch_pool_row, PyExchangeRow,
        PyLiquidityPoolRow, PyPoolManagerRow,
    };
    #[pymodule_export]
    use super::snapshot::PyDatabaseSnapshot;

    // Read seams (token-id resolution + graph edition).
    #[pymodule_export]
    use super::read_seams::{db_fetch_graph_edition, db_resolve_token_ids};

    // Discovery seam: upsert/write functions + the pool-row input builders.
    #[pymodule_export]
    use super::discovery::{
        db_set_exchange_active, db_set_exchange_last_update_block, db_upsert_exchange,
        db_upsert_pool_manager, db_upsert_v2_pools, db_upsert_v3_pools, db_upsert_v4_pools,
        PyV2PoolRowInput, PyV3PoolRowInput, PyV4PoolRowInput,
    };

    // Aave analysis seam (Step B of GAXGCR): the pure `analyze_user_position`
    // math over `degenbot-aave::analysis`. Gated on `aave-updater` (the
    // feature that brings in the `degenbot-aave` dep).
    #[cfg(feature = "aave-updater")]
    #[pymodule_export]
    use super::aave_analysis::{
        analyze_aave_user_position, PyCollateralPositionData, PyDebtPositionData,
        PyUserPositionSummary,
    };
}
