//! `PyO3` seam for the Aave V3 position read-back.
//!
//! A thin `#[pyclass]` `PyDatabasePositionQuery` (Python-visible name
//! `RustDatabasePositionQuery`) holding a [`DegenbotDb`] read handle,
//! exposing the [`degenbot_db::aave`] read fns to Python.
//!
//! The core owns the joinedload graph materialization; these extract args,
//! release the GIL via `py.detach()` for the `SQLite` I/O, call core, then
//! wrap the flat records into Python `dict`s whose keys match the Python
//! `UserRecord` / `CollateralPositionRecord` / `DebtPositionRecord` field
//! names, so the Python `DatabasePositionQuery` callers (and the pure
//! `aave/analysis/core.py`) are unchanged. No business logic (three-layer
//! architecture, ADR-005).
//!
//! The Python `src/degenbot/aave/analysis/orchestrator.py::DatabasePositionQuery`
//! class is a delegating shell over this seam.

use std::path::Path;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::conversion::alloy::u256_to_py;
use crate::db::db_err_to_py;
use degenbot_db::aave::{AaveCollateralPositionRecord, AaveDebtPositionRecord, AaveUserRecord};
use degenbot_db::DegenbotDb;

/// A read-only Aave V3 position-query handle over a degenbot `SQLite` DB file.
///
/// Opens its own connection (WAL, `query_only=on`) from `database_path`; the
/// Python `DatabasePositionQuery` shell constructs one per query and delegates
/// every read to it.
#[pyclass(
    name = "RustDatabasePositionQuery",
    frozen,
    module = "degenbot._ffi.db"
)]
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
