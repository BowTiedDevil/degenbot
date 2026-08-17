//! Row pyclasses returned by the `PyBotIo` pool-builder DB seam (QVMWQC).
//!
//! Mirrors the `SQLAlchemy` ORM rows + relationships the sync pool builders
//! traverse at construction time (`v2/v3/v4_pool_builder.py`): a pool row,
//! its per-DEX subclass kind row, the `exchange` / `token0` / `token1` FK rows,
//! and the V4 pool-manager row. Each is a thin `#[pyclass]` over the
//! corresponding `degenbot-db` row type — attribute access matches the ORM
//! attributes the builders read, so the cutover is a mechanical replacement of
//! the `SQLAlchemy` lazy-load with a per-FK fetch through `PyBotIo`.

use pyo3::prelude::*;

// ── pool + subclass rows ─────────────────────────────────────────────

/// A typed `pools` table row.
///
/// Mirrors the `SQLAlchemy` `LiquidityPoolTable` scalar + FK-id columns the
/// builders hydrate relationships from: `exchange_id` / `token0_id` /
/// `token1_id` feed the per-FK fetch methods; `kind` selects the subclass row.
#[pyclass(name = "LiquidityPoolRow", module = "degenbot._ffi.db")]
pub struct PyLiquidityPoolRow {
    id: i64,
    address: String,
    chain: i64,
    kind: String,
    token0_id: i64,
    token1_id: i64,
    exchange_id: i64,
}

impl PyLiquidityPoolRow {
    pub(crate) fn from(row: degenbot_db::rows::LiquidityPoolRow) -> Self {
        Self {
            id: row.id,
            address: row.address.to_checksum(None),
            chain: row.chain,
            kind: row.kind,
            token0_id: row.token0_id,
            token1_id: row.token1_id,
            exchange_id: row.exchange_id,
        }
    }
}

#[pymethods]
impl PyLiquidityPoolRow {
    #[getter]
    fn id(&self) -> i64 {
        self.id
    }
    #[getter]
    fn address(&self) -> String {
        self.address.clone()
    }
    #[getter]
    fn chain(&self) -> i64 {
        self.chain
    }
    #[getter]
    fn kind(&self) -> String {
        self.kind.clone()
    }
    #[getter]
    fn token0_id(&self) -> i64 {
        self.token0_id
    }
    #[getter]
    fn token1_id(&self) -> i64 {
        self.token1_id
    }
    #[getter]
    fn exchange_id(&self) -> i64 {
        self.exchange_id
    }
}

// ── FK companion rows ────────────────────────────────────────────────

/// A typed `exchanges` row (`exchange` relationship hydration).
#[pyclass(name = "ExchangeRow", module = "degenbot._ffi.db")]
pub struct PyExchangeRow {
    id: i64,
    chain_id: i64,
    name: String,
    active: bool,
    last_update_block: Option<i64>,
    factory: String,
    deployer: Option<String>,
}

impl PyExchangeRow {
    pub(crate) fn from(row: degenbot_db::rows::ExchangeRow) -> Self {
        Self {
            id: row.id,
            chain_id: row.chain_id,
            name: row.name,
            active: row.active,
            last_update_block: row.last_update_block,
            factory: row.factory.to_checksum(None),
            deployer: row.deployer.map(|a| a.to_checksum(None)),
        }
    }
}

#[pymethods]
impl PyExchangeRow {
    #[getter]
    fn id(&self) -> i64 {
        self.id
    }
    #[getter]
    fn chain_id(&self) -> i64 {
        self.chain_id
    }
    #[getter]
    fn name(&self) -> String {
        self.name.clone()
    }
    #[getter]
    fn active(&self) -> bool {
        self.active
    }
    #[getter]
    fn last_update_block(&self) -> Option<i64> {
        self.last_update_block
    }
    #[getter]
    fn factory(&self) -> String {
        self.factory.clone()
    }
    #[getter]
    fn deployer(&self) -> Option<String> {
        self.deployer.clone()
    }
}

// ── V4 pool-manager row ───────────────────────────────────────────────

/// A `pool_managers` row (V4). The V4 builder resolves its pool manager by
/// `(address, chain)` to obtain the `id` (for the V4 pool join) + the
/// `state_view` contract address.
#[pyclass(name = "PoolManagerRow", module = "degenbot._ffi.db")]
pub struct PyPoolManagerRow {
    id: i64,
    address: String,
    chain: i64,
    kind: String,
    state_view: Option<String>,
    exchange_id: i64,
}

impl PyPoolManagerRow {
    pub(crate) fn from(row: degenbot_db::rows::PoolManagerRow) -> Self {
        Self {
            id: row.id,
            address: row.address.to_checksum(None),
            chain: row.chain,
            kind: row.kind,
            state_view: row.state_view.map(|a| a.to_checksum(None)),
            exchange_id: row.exchange_id,
        }
    }
}

#[pymethods]
impl PyPoolManagerRow {
    #[getter]
    fn id(&self) -> i64 {
        self.id
    }
    #[getter]
    fn address(&self) -> String {
        self.address.clone()
    }
    #[getter]
    fn chain(&self) -> i64 {
        self.chain
    }
    #[getter]
    fn kind(&self) -> String {
        self.kind.clone()
    }
    #[getter]
    fn state_view(&self) -> Option<String> {
        self.state_view.clone()
    }
    #[getter]
    fn exchange_id(&self) -> i64 {
        self.exchange_id
    }
}

/// `degenbot._ffi.db.db_fetch_exchange(database_path, exchange_id) -> ExchangeRow | None`
///
/// Module-level exchange-row read by FK id. The `cli/pool.py::pool_update`
/// discovery loop reads `last_update_block` ground-truth here (a fresh
/// `DegenbotDb::open` per call → a fresh WAL snapshot) rather than trusting
/// the long-lived `SQLAlchemy` session's stale ORM cache: the stamp is written
/// by the Rust `db_set_exchange_last_update_block` seam on its own
/// connection, which the `SQLAlchemy` read snapshot cannot see. Mirrors
/// [`db_fetch_pool_row`] (same `database_path` + fresh-open pattern).
#[pyfunction]
pub(crate) fn db_fetch_exchange(
    py: Python<'_>,
    database_path: &str,
    exchange_id: i64,
) -> PyResult<Option<Py<PyExchangeRow>>> {
    use std::path::PathBuf;
    let path = PathBuf::from(database_path);
    let row = py
        .detach(|| {
            let (db, _state) =
                degenbot_db::DegenbotDb::open(&path).map_err(|e| crate::db::db_err_to_py(&e))?;
            db.fetch_exchange(exchange_id)
                .map_err(|e| crate::db::db_err_to_py(&e))
        })?
        .map(PyExchangeRow::from);
    match row {
        Some(r) => Ok(Some(Py::new(py, r)?)),
        None => Ok(None),
    }
}

/// `degenbot._ffi.db.db_fetch_exchange_by_name(database_path, chain_id, name)
/// -> ExchangeRow | None`
///
/// The by-name companion to [`db_fetch_exchange`] — the `(chain_id, name)`
/// lookup the `cli/exchange.py` `deactivate` commands use to resolve the row
/// to flip `active=False` on. Same fresh-open read path (`DegenbotDb::open` →
/// a fresh WAL snapshot). Raises ``ValueError`` on a DB failure.
#[pyfunction]
pub(crate) fn db_fetch_exchange_by_name(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    name: &str,
) -> PyResult<Option<Py<PyExchangeRow>>> {
    use std::path::PathBuf;
    let path = PathBuf::from(database_path);
    let row = py
        .detach(|| {
            let (db, _state) =
                degenbot_db::DegenbotDb::open(&path).map_err(|e| crate::db::db_err_to_py(&e))?;
            db.fetch_exchange_by_name(chain_id, name)
                .map_err(|e| crate::db::db_err_to_py(&e))
        })?
        .map(PyExchangeRow::from);
    match row {
        Some(r) => Ok(Some(Py::new(py, r)?)),
        None => Ok(None),
    }
}
///
/// Module-level pool-row read by `(chain_id, address)` (QJSCA5 §4.3) — the V3
/// `apply_v3_liquidity_updates` shell uses this to fetch the pool's
/// `exchange_id` for the `exchanges_in_scope` precondition before delegating
/// the math+persist to [`super::liquidity_updater::db_apply_v3_liquidity_updates`].
/// Keeps the scope-check orchestration in Python + the apply core in Rust.
#[pyfunction]
pub(crate) fn db_fetch_pool_row(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    address: &str,
) -> PyResult<Option<Py<PyLiquidityPoolRow>>> {
    use std::path::PathBuf;
    let path = PathBuf::from(database_path);
    let addr = crate::bot::py_bot_io::parse_address_for_call(address)?;
    let row = py
        .detach(|| {
            let (db, _state) =
                degenbot_db::DegenbotDb::open(&path).map_err(|e| crate::db::db_err_to_py(&e))?;
            db.fetch_pool_by_address(alloy::primitives::Address::from(addr), chain_id)
                .map_err(|e| crate::db::db_err_to_py(&e))
        })?
        .map(PyLiquidityPoolRow::from);
    match row {
        Some(r) => Ok(Some(Py::new(py, r)?)),
        None => Ok(None),
    }
}
