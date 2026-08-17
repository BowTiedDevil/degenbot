//! `PyO3` seam for the Aave V3 position read-back + the Aave V3 DB writers.
//!
//! # Read-back (`PyDatabasePositionQuery`)
//!
//! A thin `#[pyclass]` `PyDatabasePositionQuery` holding a [`DegenbotDb`] read
//! handle, exposing the [`degenbot_db::aave`] read fns to Python.
//!
//! The core owns the joinedload graph materialization; these extract args,
//! release the GIL via `py.detach()` for the `SQLite` I/O, call core, then wrap
//! the flat records into Python `dict`s whose keys match the Python
//! `UserRecord` / `CollateralPositionRecord` / `DebtPositionRecord` field
//! names, so the Python `DatabasePositionQuery` callers (and the pure
//! `aave/analysis/core.py`) are unchanged. No business logic (three-layer
//! architecture, ADR-005).
//!
//! The Python `src/degenbot/aave/analysis/orchestrator.py::DatabasePositionQuery`
//! class becomes a delegating shell over this seam.
//!
//! # Writers (Sub-step B of AZGJUN/RQXEKH)
//!
//! Module-level `#[pyfunction]`s wrapping the 14 already-landed Rust core
//! writer fns in [`degenbot_db::write`] (the `get_or_create_*` × 7 + `apply_*`
//! × 7) + [`degenbot_db::write::decode_reserve_configuration_bitmap`]. Each
//! writer takes a `database_path: &str`, opens its own write handle via
//! [`DegenbotDb::open_for_writes`] per call (NOT held across calls — mirrors
//! the `liquidity_updater` precedent), and releases the GIL across the DB
//! I/O via `py.detach()`. No business logic: arg extract → `py.detach()` →
//! core call → result wrap.
//!
//! The RPC-coupling carve-out is preserved: `get_or_create_erc20_token` +
//! `get_or_create_user` take caller-supplied `Option` metadata/`gho_discount`
//! params; the Python driver stays the RPC authority (`degenbot-db` has no
//! `degenbot-rpc` dep — ADR-005 standalone).

use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};

use crate::conversion::alloy::{extract_python_u256, u256_to_py};
use crate::db::db_err_to_py;
use degenbot_db::aave::{AaveCollateralPositionRecord, AaveDebtPositionRecord, AaveUserRecord};
use degenbot_db::write::decode_reserve_configuration_bitmap;
use degenbot_db::DegenbotDb;

/// A read-only Aave V3 position-query handle over a degenbot `SQLite` DB file.
///
/// Opens its own connection (WAL, `query_only=on`) from `database_path`; the
/// Python `DatabasePositionQuery` shell constructs one per query and delegates
/// every read to it.
#[pyclass(frozen, module = "degenbot._ffi.db")]
pub struct PyDatabasePositionQuery {
    db: DegenbotDb,
}

#[pymethods]
impl PyDatabasePositionQuery {
    /// Open a `query_only` read handle over `database_path`.
    ///
    /// Raises:
    ///     `ValueError`: On a connection / PRAGMA / schema-gate failure.
    #[new]
    fn new(database_path: &str) -> PyResult<Self> {
        let (db, _state) =
            DegenbotDb::open(Path::new(database_path)).map_err(|e| db_err_to_py(&e))?;
        Ok(Self { db })
    }

    /// `get_users_with_debt(market_id: int, limit: int | None = None) ->
    /// list[dict]` — one dict per user (keys: `id`, `address`, `market_id`,
    /// `e_mode`, `is_isolation_mode`, `isolation_mode_debt`,
    /// `isolation_debt_ceiling`).
    fn get_users_with_debt<'py>(
        &self,
        py: Python<'py>,
        market_id: i64,
        limit: Option<i64>,
    ) -> PyResult<Bound<'py, PyList>> {
        let rows = py
            .detach(|| self.db.fetch_aave_users_with_debt(market_id, limit))
            .map_err(|e| db_err_to_py(&e))?;
        let out = PyList::empty(py);
        for r in rows {
            out.append(user_record_to_py(py, &r)?)?;
        }
        Ok(out)
    }

    /// `get_collateral_positions(user_id: int) -> list[dict]` — keys:
    /// `asset_id`, `balance`, `underlying_address`, `underlying_symbol`,
    /// `liquidity_index`, `e_mode_category_id`, `asset_lt`, `asset_ltv`,
    /// `emode_lt`, `emode_ltv`.
    fn get_collateral_positions<'py>(
        &self,
        py: Python<'py>,
        user_id: i64,
    ) -> PyResult<Bound<'py, PyList>> {
        let rows = py
            .detach(|| self.db.fetch_aave_collateral_positions(user_id))
            .map_err(|e| db_err_to_py(&e))?;
        let out = PyList::empty(py);
        for r in rows {
            out.append(collateral_record_to_py(py, &r)?)?;
        }
        Ok(out)
    }

    /// `get_debt_positions(user_id: int) -> list[dict]` — keys: `asset_id`,
    /// `balance`, `underlying_address`, `underlying_symbol`, `borrow_index`,
    /// `e_mode_category_id`.
    fn get_debt_positions<'py>(
        &self,
        py: Python<'py>,
        user_id: i64,
    ) -> PyResult<Bound<'py, PyList>> {
        let rows = py
            .detach(|| self.db.fetch_aave_debt_positions(user_id))
            .map_err(|e| db_err_to_py(&e))?;
        let out = PyList::empty(py);
        for r in rows {
            out.append(debt_record_to_py(py, &r)?)?;
        }
        Ok(out)
    }

    /// `get_collateral_config_map(user_id: int) -> dict[int, bool]` — the
    /// `asset_id` to `enabled` map.
    fn get_collateral_config_map<'py>(
        &self,
        py: Python<'py>,
        user_id: i64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let map = py
            .detach(|| self.db.fetch_aave_collateral_config_map(user_id))
            .map_err(|e| db_err_to_py(&e))?;
        let out = PyDict::new(py);
        for (asset_id, enabled) in map {
            out.set_item(asset_id, enabled)?;
        }
        Ok(out)
    }

    /// `get_oracle_address(market_id: int) -> str | None` — the
    /// `PRICE_ORACLE` contract address.
    fn get_oracle_address(&self, py: Python<'_>, market_id: i64) -> PyResult<Option<String>> {
        py.detach(|| self.db.fetch_aave_oracle_address(market_id))
            .map_err(|e| db_err_to_py(&e))
    }

    /// `get_asset_addresses(market_id: int) -> list[str]` — distinct
    /// underlying-token addresses (the Python shell wraps into a `set`).
    fn get_asset_addresses(&self, py: Python<'_>, market_id: i64) -> PyResult<Vec<String>> {
        py.detach(|| self.db.fetch_aave_asset_addresses(market_id))
            .map_err(|e| db_err_to_py(&e))
    }
}

/// Build the Python dict mirroring `UserRecord`.
fn user_record_to_py<'py>(py: Python<'py>, r: &AaveUserRecord) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("id", r.id)?;
    d.set_item("address", r.address.clone())?;
    d.set_item("market_id", r.market_id)?;
    d.set_item("e_mode", r.e_mode)?;
    d.set_item("is_isolation_mode", r.is_isolation_mode)?;
    d.set_item(
        "isolation_mode_debt",
        u256_to_py(py, &r.isolation_mode_debt)?,
    )?;
    match r.isolation_debt_ceiling {
        Some(ref v) => d.set_item("isolation_debt_ceiling", u256_to_py(py, v)?)?,
        None => d.set_item("isolation_debt_ceiling", py.None())?,
    }
    Ok(d)
}

/// Build the Python dict mirroring `CollateralPositionRecord`.
fn collateral_record_to_py<'py>(
    py: Python<'py>,
    r: &AaveCollateralPositionRecord,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("asset_id", r.asset_id)?;
    d.set_item("balance", u256_to_py(py, &r.balance)?)?;
    d.set_item("underlying_address", r.underlying_address.clone())?;
    match &r.underlying_symbol {
        Some(s) => d.set_item("underlying_symbol", s.clone())?,
        None => d.set_item("underlying_symbol", py.None())?,
    }
    d.set_item("liquidity_index", u256_to_py(py, &r.liquidity_index)?)?;
    match r.e_mode_category_id {
        Some(v) => d.set_item("e_mode_category_id", v)?,
        None => d.set_item("e_mode_category_id", py.None())?,
    }
    d.set_item("asset_lt", r.asset_lt)?;
    d.set_item("asset_ltv", r.asset_ltv)?;
    match r.emode_lt {
        Some(v) => d.set_item("emode_lt", v)?,
        None => d.set_item("emode_lt", py.None())?,
    }
    match r.emode_ltv {
        Some(v) => d.set_item("emode_ltv", v)?,
        None => d.set_item("emode_ltv", py.None())?,
    }
    Ok(d)
}

/// Build the Python dict mirroring `DebtPositionRecord`.
fn debt_record_to_py<'py>(
    py: Python<'py>,
    r: &AaveDebtPositionRecord,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("asset_id", r.asset_id)?;
    d.set_item("balance", u256_to_py(py, &r.balance)?)?;
    d.set_item("underlying_address", r.underlying_address.clone())?;
    match &r.underlying_symbol {
        Some(s) => d.set_item("underlying_symbol", s.clone())?,
        None => d.set_item("underlying_symbol", py.None())?,
    }
    d.set_item("borrow_index", u256_to_py(py, &r.borrow_index)?)?;
    match r.e_mode_category_id {
        Some(v) => d.set_item("e_mode_category_id", v)?,
        None => d.set_item("e_mode_category_id", py.None())?,
    }
    Ok(d)
}

// =========================================================================
// Writers (Sub-step B of AZGJUN/RQXEKH)
// =========================================================================
//
// Module-level `#[pyfunction]` wrappers over `degenbot_db::write`'s 14 writer
// fns + `decode_reserve_configuration_bitmap`. Mirror the `liquidity_updater`
// precedent: a `database_path: &str` arg, a per-call
// `DegenbotDb::open_for_writes(&path)`, `py.detach()` around the substrate
// I/O, `db_err_to_py` mapping. No business logic.
//
// The `decode_reserve_configuration_bitmap` dict serialization matches the
// Python `event_handlers.py::_decode_reserve_configuration_bitmap` oracle's
// dict keys 1:1 (the `ReserveConfiguration` field names), so the Python
// driver + the parity test are unchanged.

/// `degenbot._ffi.db.db_get_or_create_e_mode_category(database_path, market_id, category_id) -> int`
///
/// Get-or-create an `aave_v3_emode_categories` row by `(market_id,
/// category_id)`. On create, inserts the Python ORM defaults (`label=""`,
/// `ltv=0`, `liquidation_threshold=0`, `liquidation_bonus=0`). Returns the
/// row `id`. Raises `ValueError` on a DB failure.
#[pyfunction]
pub(crate) fn db_get_or_create_e_mode_category(
    py: Python<'_>,
    database_path: &str,
    market_id: i64,
    category_id: i64,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.get_or_create_e_mode_category(market_id, category_id)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_get_or_create_asset_config(database_path, asset_id) -> int`
///
/// Get-or-create an `aave_v3_asset_configs` row by `asset_id`. On create,
/// inserts the Python ORM defaults (all zero/`false`/`None`). Returns the
/// row `id`. Raises `ValueError` on a DB failure.
#[pyfunction]
pub(crate) fn db_get_or_create_asset_config(
    py: Python<'_>,
    database_path: &str,
    asset_id: i64,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.get_or_create_asset_config(asset_id)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_get_or_create_user_collateral_config(database_path, user_id, asset_id) -> int`
///
/// Get-or-create an `aave_v3_user_collateral_configs` row by `(user_id,
/// asset_id)`. On create, inserts `enabled=False` (the Python default).
/// Returns the row `id`. Raises `ValueError` on a DB failure.
#[pyfunction]
pub(crate) fn db_get_or_create_user_collateral_config(
    py: Python<'_>,
    database_path: &str,
    user_id: i64,
    asset_id: i64,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.get_or_create_user_collateral_config(user_id, asset_id)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_get_or_create_user(database_path, market_id, address, gho_discount) -> int`
///
/// Get-or-create an `aave_v3_users` row by `(market_id, address)`. On create,
/// inserts the Python ORM defaults (`e_mode=0`, `gho_discount=<arg>`,
/// `stk_aave_balance=NULL`, `isolation_mode_collateral_asset_id=NULL`,
/// `isolation_mode_debt="0"`). `gho_discount` is caller-supplied — the
/// Python driver RPC-fetches the GHO discount (that fetch is `stays-python`);
/// pass `0` for non-GHO markets. Returns the row `id`. Raises `ValueError` on
/// a DB failure.
#[pyfunction]
pub(crate) fn db_get_or_create_user(
    py: Python<'_>,
    database_path: &str,
    market_id: i64,
    address: &str,
    gho_discount: i64,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.get_or_create_user(market_id, address, gho_discount)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_get_or_create_erc20_token(database_path, chain, address, name=None, symbol=None, decimals=None) -> int`
///
/// Get-or-create an `erc20_tokens` row by `(chain, address)`. On create,
/// inserts caller-supplied metadata (`name` / `symbol` / `decimals`); the
/// Python path RPC-fetches these via `_fetch_erc20_token_metadata` — that
/// fetch is `stays-python` (the driver computes + passes them in). Pass
/// `None` for each to leave the column `NULL` (matches the Python "metadata
/// fetch returned None" trajectory). A second call returns the existing row
/// id without overwriting metadata.
///
/// (Metadata write-back — `update_erc20_token_metadata` — already lives on
/// `PyBotIo`; do NOT re-bind it here.) Raises `ValueError` on a DB failure.
#[pyfunction]
#[pyo3(signature = (database_path, chain, address, name=None, symbol=None, decimals=None))]
pub(crate) fn db_get_or_create_erc20_token(
    py: Python<'_>,
    database_path: &str,
    chain: i64,
    address: &str,
    name: Option<&str>,
    symbol: Option<&str>,
    decimals: Option<i64>,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.get_or_create_erc20_token(chain, address, name, symbol, decimals)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_get_or_create_collateral_position(database_path, user_id, asset_id) -> int`
///
/// Get-or-create an `aave_v3_collateral_positions` row by `(user_id,
/// asset_id)`. On create, inserts `balance='0'`, `last_index=NULL`. Returns
/// the row `id`. Raises `ValueError` on a DB failure.
#[pyfunction]
pub(crate) fn db_get_or_create_collateral_position(
    py: Python<'_>,
    database_path: &str,
    user_id: i64,
    asset_id: i64,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.get_or_create_collateral_position(user_id, asset_id)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_get_or_create_debt_position(database_path, user_id, asset_id) -> int`
///
/// Get-or-create an `aave_v3_debt_positions` row by `(user_id, asset_id)`. On
/// create, inserts `balance='0'`, `last_index=NULL`. Returns the row `id`.
/// Raises `ValueError` on a DB failure.
#[pyfunction]
pub(crate) fn db_get_or_create_debt_position(
    py: Python<'_>,
    database_path: &str,
    user_id: i64,
    asset_id: i64,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.get_or_create_debt_position(user_id, asset_id)
            .map_err(|e| db_err_to_py(&e))
    })
}

// ── the per-event apply fns ───────────────────────────────────────────────

/// `degenbot._ffi.db.db_apply_collateral_configuration_changed(database_path, asset_id, config_bitmap) -> int`
///
/// Apply a `CollateralConfigurationChanged` event's decoded config bitmap to
/// the asset's `aave_v3_asset_configs` row. `config_bitmap` is the raw
/// `uint256` the Python driver RPC-fetches via `Pool.getConfiguration` (the
/// fetch is `stays-python`); decoded via
/// [`degenbot_db::write::decode_reserve_configuration_bitmap`]. On a new row,
/// inserts every decoded field; on an existing row, `UPDATE`s every field.
/// Returns the row `id`. Raises `ValueError` on a DB failure or if the
/// `config_bitmap` isn't a non-negative 256-bit integer.
#[pyfunction]
pub(crate) fn db_apply_collateral_configuration_changed(
    py: Python<'_>,
    database_path: &str,
    asset_id: i64,
    config_bitmap: &Bound<'_, PyAny>,
) -> PyResult<i64> {
    let bitmap = extract_python_u256(config_bitmap)?;
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.apply_collateral_configuration_changed(asset_id, bitmap)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_apply_e_mode_category_added(database_path, market_id, category_id, ltv, liquidation_threshold, liquidation_bonus, price_source=None, label='') -> int`
///
/// Apply an `EModeCategoryAdded` event's decoded fields to the
/// `aave_v3_emode_categories` row. On create, inserts `(label, ltv,
/// liquidation_threshold, liquidation_bonus, price_source)`; on existing,
/// `UPDATE`s the same five fields. `price_source` is the oracle address
/// (`None` when zero / no oracle). Returns the row `id`. Raises `ValueError`
/// on a DB failure.
#[pyfunction]
#[pyo3(signature = (database_path, market_id, category_id, ltv, liquidation_threshold, liquidation_bonus, price_source=None, label=""))]
#[expect(clippy::too_many_arguments)] // mirrors the Python event arg list 1:1
pub(crate) fn db_apply_e_mode_category_added(
    py: Python<'_>,
    database_path: &str,
    market_id: i64,
    category_id: i64,
    ltv: u64,
    liquidation_threshold: u64,
    liquidation_bonus: u64,
    price_source: Option<&str>,
    label: &str,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.apply_e_mode_category_added(
            market_id,
            category_id,
            ltv,
            liquidation_threshold,
            liquidation_bonus,
            price_source,
            label,
        )
        .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_apply_emode_asset_category_changed(database_path, asset_id, new_category_id) -> int`
///
/// Apply an `EModeAssetCategoryChanged` event's category assignment to the
/// asset's `e_mode_category_id` (the older Aave variant). Unconditionally
/// sets `e_mode_category_id` to the new category (`None` when the category id
/// is `0`). Returns the row `id`. Raises `ValueError` on a DB failure.
#[pyfunction]
pub(crate) fn db_apply_emode_asset_category_changed(
    py: Python<'_>,
    database_path: &str,
    asset_id: i64,
    new_category_id: i64,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.apply_emode_asset_category_changed(asset_id, new_category_id)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_apply_asset_collateral_in_emode_changed(database_path, asset_id, category_id, is_collateral) -> int`
///
/// Apply an `AssetCollateralInEModeChanged` event's category assignment (the
/// newer Aave v3.4+ variant). Sets `e_mode_category_id` to the category ONLY
/// when `is_collateral && category_id > 0`; otherwise the row is left
/// unchanged (the Python `elif` branch). Returns the row `id`. Raises
/// `ValueError` on a DB failure.
#[pyfunction]
pub(crate) fn db_apply_asset_collateral_in_emode_changed(
    py: Python<'_>,
    database_path: &str,
    asset_id: i64,
    category_id: i64,
    is_collateral: bool,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.apply_asset_collateral_in_emode_changed(asset_id, category_id, is_collateral)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_apply_reserve_used_as_collateral(database_path, user_id, asset_id, enabled) -> int`
///
/// Apply a `ReserveUsedAsCollateralEnabled`/`Disabled` event: set the
/// `aave_v3_user_collateral_configs.enabled` flag (`True` for enabled,
/// `False` for disabled — both collapse to the same handler). On create,
/// inserts with the given `enabled` value; on existing, `UPDATE`s the flag.
/// Returns the row `id`. Raises `ValueError` on a DB failure.
#[pyfunction]
pub(crate) fn db_apply_reserve_used_as_collateral(
    py: Python<'_>,
    database_path: &str,
    user_id: i64,
    asset_id: i64,
    enabled: bool,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.apply_reserve_used_as_collateral(user_id, asset_id, enabled)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_apply_user_e_mode_set(database_path, user_id, e_mode) -> int`
///
/// Apply a `UserEModeSet` event: set the user's `aave_v3_users.e_mode`
/// column. The user row must already exist (the Python path
/// `get_or_create_user`s first; the driver passes the existing `user_id`).
/// Returns the `user_id`. Raises `ValueError` on a DB failure.
#[pyfunction]
pub(crate) fn db_apply_user_e_mode_set(
    py: Python<'_>,
    database_path: &str,
    user_id: i64,
    e_mode: i64,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.apply_user_e_mode_set(user_id, e_mode)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_apply_price_oracle_updated(database_path, market_id, new_oracle_address) -> int`
///
/// Apply a `PriceOracleUpdated` event: register the new `PRICE_ORACLE`
/// `aave_v3_contracts` row for `market_id`. Upserts — if a `PRICE_ORACLE` row
/// exists it `UPDATE`s the address, else it inserts. Returns the row `id`.
/// Raises `ValueError` on a DB failure.
#[pyfunction]
pub(crate) fn db_apply_price_oracle_updated(
    py: Python<'_>,
    database_path: &str,
    market_id: i64,
    new_oracle_address: &str,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.apply_price_oracle_updated(market_id, new_oracle_address)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot._ffi.db.db_apply_asset_source_updated(database_path, asset_id, source_address) -> int`
///
/// Apply an `AssetSourceUpdated` event: set the asset's
/// `aave_v3_assets.price_source` column. The `asset_id` must already exist
/// (the Python `assert asset is not None`). Returns the `asset_id`. Raises
/// `ValueError` on a DB failure.
#[pyfunction]
pub(crate) fn db_apply_asset_source_updated(
    py: Python<'_>,
    database_path: &str,
    asset_id: i64,
    source_address: &str,
) -> PyResult<i64> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) = DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.apply_asset_source_updated(asset_id, source_address)
            .map_err(|e| db_err_to_py(&e))
    })
}

// ── the pure CPU bit-decode (no I/O — `py.detach()` for consistency) ──────

/// `degenbot._ffi.db.db_decode_reserve_configuration_bitmap(config_bitmap) -> dict`
///
/// Decode the Aave V3 reserve-configuration `uint256` bitmap into the typed
/// dict. Pure CPU — `py.detach()` is used for consistency with the writer
/// seam (no I/O happens). The returned dict's keys match the Python
/// `_decode_reserve_configuration_bitmap` oracle 1:1 (`ltv`,
/// `liquidation_threshold`, `liquidation_bonus`, `decimals`, `is_active`,
/// `is_frozen`, `borrowing_enabled`, `stable_rate_borrowing_enabled`,
/// `reserve_factor`, `borrow_cap`, `supply_cap`, `debt_ceiling`,
/// `liquidation_protocol_fee`, `unbacked_mint_cap`, `e_mode_category_id`
/// (`None` when the decoded byte is `0`), `flash_loan_enabled`,
/// `isolation_mode`, `borrowable_in_isolation`), so the Python driver + the
/// parity test are unchanged.
///
/// `config_bitmap` is the raw `uint256` returned by the Pool contract's
/// `getConfiguration(address)`; the caller (Python driver) RPC-fetches it.
/// Raises `ValueError` if the `config_bitmap` isn't a non-negative 256-bit
/// integer.
#[pyfunction]
pub(crate) fn db_decode_reserve_configuration_bitmap(
    py: Python<'_>,
    config_bitmap: &Bound<'_, PyAny>,
) -> PyResult<Py<PyDict>> {
    // Extract the int under the GIL (touches Python), then detach the pure
    // CPU bit-decode (no I/O), then build the dict (GIL-held).
    let bitmap = extract_python_u256(config_bitmap)?;
    let cfg = py.detach(|| decode_reserve_configuration_bitmap(bitmap));
    reserve_config_to_py(py, &cfg)
}

/// Build the Python dict mirroring the `_decode_reserve_configuration_bitmap`
/// oracle (`event_handlers.py`). The keys match the `ReserveConfiguration`
/// field names 1:1; `e_mode_category_id` is `None` when the decoded byte was
/// `0`; `debt_ceiling` is returned as a Python `int` (the decoded `u64`).
fn reserve_config_to_py(
    py: Python<'_>,
    r: &degenbot_db::write::ReserveConfiguration,
) -> PyResult<Py<PyDict>> {
    let d = PyDict::new(py);
    d.set_item("ltv", r.ltv)?;
    d.set_item("liquidation_threshold", r.liquidation_threshold)?;
    d.set_item("liquidation_bonus", r.liquidation_bonus)?;
    d.set_item("decimals", r.decimals)?;
    d.set_item("is_active", r.is_active)?;
    d.set_item("is_frozen", r.is_frozen)?;
    d.set_item("borrowing_enabled", r.borrowing_enabled)?;
    d.set_item(
        "stable_rate_borrowing_enabled",
        r.stable_rate_borrowing_enabled,
    )?;
    d.set_item("reserve_factor", r.reserve_factor)?;
    d.set_item("borrow_cap", r.borrow_cap)?;
    d.set_item("supply_cap", r.supply_cap)?;
    d.set_item("debt_ceiling", r.debt_ceiling)?;
    d.set_item("liquidation_protocol_fee", r.liquidation_protocol_fee)?;
    d.set_item("unbacked_mint_cap", r.unbacked_mint_cap)?;
    match r.e_mode_category_id {
        Some(v) => d.set_item("e_mode_category_id", v)?,
        None => d.set_item("e_mode_category_id", py.None())?,
    }
    d.set_item("flash_loan_enabled", r.flash_loan_enabled)?;
    d.set_item("isolation_mode", r.isolation_mode)?;
    d.set_item("borrowable_in_isolation", r.borrowable_in_isolation)?;
    Ok(d.unbind())
}
