//! `PyO3` seam for the pool discovery writers — WR7EA6 (split out of QJSCA5).
//!
//! Wraps `degenbot-db`'s `discovery` substrate
//! ([`degenbot_db::DegenbotDb::upsert_v2_pools`] / [`upsert_v3_pools`] /
//! [`upsert_v4_pools`] / [`set_exchange_last_update_block`]) as module-level
//! `#[pyfunction]`s taking a `database_path` (same pattern as the QJSCA5
//! `db_apply_v3/v4_liquidity_updates` seam). The Python
//! `cli/pool_updater_configs.py::update_v2/v3/v4_pools` shells decode the raw
//! `PoolCreated` `LogReceipt`s (topics/data → addresses/fee/tick-spacing) + do
//! the RPC fee lookup, then build [`PyV2PoolRowInput`] / [`PyV3PoolRowInput`]
//! / [`PyV4PoolRowInput`] lists + delegate here — the `Rust` core owns the
//! `erc20_tokens` get-or-create escalate + the polymorphic pool-row insert +
//! the `ExchangeTable.last_update_block` stamp.
//!
//! `py.detach()` wraps the DB I/O (no `GIL` held during the upsert loop).

use std::path::PathBuf;

use alloy::primitives::Address;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::address_utils::parse_address;
use crate::db::db_err_to_py;
use crate::db::pool_read::{PyExchangeRow, PyPoolManagerRow};

// ═══════════════════════════════════════════════════════════════════════════
// Python row-input pyclasses (the arg-extraction boundary)
// ═══════════════════════════════════════════════════════════════════════════

/// One V2 pool-row to upsert (WR7EA6). Mirrors [`degenbot_db::V2PoolRowInput`]:
/// the `(address, token0_address, token1_address, fee_token0, fee_token1,
/// stable)` tuple. The Python `update_v2_pools` shell decodes the
/// `PoolCreated` event + builds these records; the `Rust` core get-or-create's
/// the tokens + inserts the polymorphic base `pools` row + the subclass detail
/// row. `stable` is `None` for all V2 families except `Aerodrome` (the sole
/// V2 subclass with a `stable` column).
#[pyclass(name = "V2PoolRowInput", module = "degenbot._ffi.db")]
pub struct PyV2PoolRowInput {
    address: String,
    token0_address: String,
    token1_address: String,
    fee_token0: i64,
    fee_token1: i64,
    stable: Option<bool>,
}

#[pymethods]
impl PyV2PoolRowInput {
    #[new]
    #[pyo3(signature = (address, token0_address, token1_address, fee_token0, fee_token1, stable=None))]
    fn new(
        address: String,
        token0_address: String,
        token1_address: String,
        fee_token0: i64,
        fee_token1: i64,
        stable: Option<bool>,
    ) -> Self {
        Self {
            address,
            token0_address,
            token1_address,
            fee_token0,
            fee_token1,
            stable,
        }
    }
}

impl PyV2PoolRowInput {
    fn to_input(&self) -> PyResult<degenbot_db::V2PoolRowInput> {
        Ok(degenbot_db::V2PoolRowInput {
            address: parse_address(&self.address)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?,
            token0_address: parse_address(&self.token0_address)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?,
            token1_address: parse_address(&self.token1_address)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?,
            fee_token0: self.fee_token0,
            fee_token1: self.fee_token1,
            stable: self.stable,
        })
    }
}

/// One V3 pool-row to upsert (WR7EA6). Mirrors [`degenbot_db::V3PoolRowInput`].
#[pyclass(name = "V3PoolRowInput", module = "degenbot._ffi.db")]
pub struct PyV3PoolRowInput {
    address: String,
    token0_address: String,
    token1_address: String,
    fee: i64,
    tick_spacing: i64,
}

#[pymethods]
impl PyV3PoolRowInput {
    #[new]
    fn new(
        address: String,
        token0_address: String,
        token1_address: String,
        fee: i64,
        tick_spacing: i64,
    ) -> Self {
        Self {
            address,
            token0_address,
            token1_address,
            fee,
            tick_spacing,
        }
    }
}

impl PyV3PoolRowInput {
    fn to_input(&self) -> PyResult<degenbot_db::V3PoolRowInput> {
        Ok(degenbot_db::V3PoolRowInput {
            address: parse_address(&self.address)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?,
            token0_address: parse_address(&self.token0_address)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?,
            token1_address: parse_address(&self.token1_address)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?,
            fee: self.fee,
            tick_spacing: self.tick_spacing,
        })
    }
}

/// One V4 pool-row to upsert (WR7EA6). Mirrors [`degenbot_db::V4PoolRowInput`].
/// The `pool_id` / `manager_id` is resolved inside the `Rust` core from the
/// passed `pool_manager_address` (one `SELECT` per batch).
#[pyclass(name = "V4PoolRowInput", module = "degenbot._ffi.db")]
pub struct PyV4PoolRowInput {
    pool_hash: String,
    hooks: String,
    currency0_address: String,
    currency1_address: String,
    fee: i64,
    tick_spacing: i64,
}

#[pymethods]
impl PyV4PoolRowInput {
    #[new]
    fn new(
        pool_hash: String,
        hooks: String,
        currency0_address: String,
        currency1_address: String,
        fee: i64,
        tick_spacing: i64,
    ) -> Self {
        Self {
            pool_hash,
            hooks,
            currency0_address,
            currency1_address,
            fee,
            tick_spacing,
        }
    }
}

impl PyV4PoolRowInput {
    fn to_input(&self) -> PyResult<degenbot_db::V4PoolRowInput> {
        Ok(degenbot_db::V4PoolRowInput {
            pool_hash: self.pool_hash.clone(),
            hooks: parse_address(&self.hooks)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?,
            currency0_address: parse_address(&self.currency0_address)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?,
            currency1_address: parse_address(&self.currency1_address)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?,
            fee: self.fee,
            tick_spacing: self.tick_spacing,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PyO3 wrapper functions
// ═══════════════════════════════════════════════════════════════════════════

/// `degenbot_rs.db_upsert_v2_pools(database_path, chain_id, kind, exchange_id,
/// fee_denominator, rows) -> None`
///
/// Insert a batch of V2 pool rows. The `Rust` core get-or-create's the two
/// `erc20_tokens` per row + inserts the polymorphic base `pools` row (kind) +
/// the subclass detail row (`fee_token0`/`fee_token1`/`fee_denominator`, +
/// `stable` for `aerodrome_v2`). Raises `ValueError` if `kind` is not a known
/// V2 family discriminator (mirrors the Python
/// `config.database_type` dispatch). The address uniqueness constraint raises
/// `ValueError` (`SQLITE_CONSTRAINT`) on a duplicate `(address, chain)`,
/// matching the Python `session.add` `IntegrityError` trajectory.
#[pyfunction]
pub(crate) fn db_upsert_v2_pools(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    kind: &str,
    exchange_id: i64,
    fee_denominator: i64,
    rows: Vec<Py<PyV2PoolRowInput>>,
) -> PyResult<()> {
    let path = PathBuf::from(database_path);
    let rust_rows: Vec<degenbot_db::V2PoolRowInput> = rows
        .into_iter()
        .map(|r| r.borrow(py).to_input())
        .collect::<PyResult<_>>()?;
    py.detach(|| {
        let (db, _state) =
            degenbot_db::DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.upsert_v2_pools(chain_id, kind, exchange_id, fee_denominator, &rust_rows)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot_rs.db_upsert_v3_pools(database_path, chain_id, kind, exchange_id,
/// fee_denominator, rows) -> None`
///
/// Insert a batch of V3 pool rows. Same shape as [`db_upsert_v2_pools`];
/// subclass detail row carries `tick_spacing` + the fee columns
/// (`liquidity_update_block`/`log_index` left `NULL` until the liquidity
/// updater stamps them). Raises `ValueError` if `kind` is not a V3 family.
#[pyfunction]
pub(crate) fn db_upsert_v3_pools(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    kind: &str,
    exchange_id: i64,
    fee_denominator: i64,
    rows: Vec<Py<PyV3PoolRowInput>>,
) -> PyResult<()> {
    let path = PathBuf::from(database_path);
    let rust_rows: Vec<degenbot_db::V3PoolRowInput> = rows
        .into_iter()
        .map(|r| r.borrow(py).to_input())
        .collect::<PyResult<_>>()?;
    py.detach(|| {
        let (db, _state) =
            degenbot_db::DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.upsert_v3_pools(chain_id, kind, exchange_id, fee_denominator, &rust_rows)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot_rs.db_upsert_v4_pools(database_path, chain_id, pool_manager_address,
/// fee_denominator, rows) -> None`
///
/// Insert a batch of V4 pool rows. The `Rust` core resolves the
/// `PoolManagerTable` id from `(chain_id, pool_manager_address)` (one
/// `SELECT`), then per row: get-or-create the two currency tokens + insert
/// the `managed_pools` polymorphic base (`kind='uniswap_v4', manager_id`) +
/// the `uniswap_v4_pools` detail row. Raises `ValueError` if no
/// `PoolManager` row matches (mirrors the Python
/// `assert manager_in_db is not None`).
#[pyfunction]
pub(crate) fn db_upsert_v4_pools(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    pool_manager_address: &str,
    fee_denominator: i64,
    rows: Vec<Py<PyV4PoolRowInput>>,
) -> PyResult<()> {
    let path = PathBuf::from(database_path);
    let rust_rows: Vec<degenbot_db::V4PoolRowInput> = rows
        .into_iter()
        .map(|r| r.borrow(py).to_input())
        .collect::<PyResult<_>>()?;
    py.detach(|| {
        let (db, _state) =
            degenbot_db::DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.upsert_v4_pools(chain_id, pool_manager_address, fee_denominator, &rust_rows)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot_rs.db_set_exchange_last_update_block(database_path, chain_id,
/// exchange_id, block) -> None`
///
/// Stamp an `ExchangeTable.last_update_block`. Port of the
/// `exchange.last_update_block = working_end_block` trajectory in
/// `cli/pool.py::pool_update` (the discovery-writer close-out stamp).
#[pyfunction]
pub(crate) fn db_set_exchange_last_update_block(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    exchange_id: i64,
    block: i64,
) -> PyResult<()> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) =
            degenbot_db::DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.set_exchange_last_update_block(chain_id, exchange_id, block)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot_rs.db_upsert_exchange(database_path, chain_id, name, factory,
/// deployer) -> ExchangeRow`
///
/// Resolve an `exchanges` row by `(chain_id, name)`, inserting a new
/// `active=False`, `last_update_block=None` row if none exists (the substrate
/// the `exchange activate/deactivate` CLI delegates to for the get-or-create
/// step). If a row already exists it is returned UNCHANGED — `active` is NOT
/// touched here (flipping active is a separate concern, see
/// [`db_set_exchange_active`]). `exchanges` has no unique index on
/// `(chain_id, name)` so this uses the explicit SELECT-then-INSERT dedup the
/// Python `cli/exchange.py` trajectory uses (no Alembic migration forced). The
/// `factory` / `deployer` are stored EIP-55-checksummed. Raises `ValueError`
/// on a parse or DB failure.
#[pyfunction]
pub(crate) fn db_upsert_exchange(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    name: &str,
    factory: &str,
    deployer: Option<&str>,
) -> PyResult<Py<PyExchangeRow>> {
    let path = PathBuf::from(database_path);
    let factory = Address::parse_checksummed(factory, None)
        .map_err(|e| PyValueError::new_err(format!("bad factory address: {e}")))?;
    let deployer = deployer
        .map(|d| {
            Address::parse_checksummed(d, None)
                .map_err(|e| PyValueError::new_err(format!("bad deployer address: {e}")))
        })
        .transpose()?;
    let row = py.detach(|| {
        let (db, _state) =
            degenbot_db::DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.upsert_exchange(chain_id, name, factory, deployer)
            .map_err(|e| db_err_to_py(&e))
    })?;
    Py::new(py, PyExchangeRow::from(row))
}

/// `degenbot_rs.db_set_exchange_active(database_path, exchange_id, active)
/// -> None`
///
/// Flip an `exchanges` row's `active` flag by id — the activate/deactivate
/// primitive the `exchange activate` / `exchange deactivate` CLI commands
/// delegate to (the Python trajectory's `exchange.active = True/False;
/// session.commit()`). Raises `ValueError` (a not-found error surfaced from
/// [`degenbot_db::DbError::MissingRow`]) if no row matches `exchange_id`.
#[pyfunction]
pub(crate) fn db_set_exchange_active(
    py: Python<'_>,
    database_path: &str,
    exchange_id: i64,
    active: bool,
) -> PyResult<()> {
    let path = PathBuf::from(database_path);
    py.detach(|| {
        let (db, _state) =
            degenbot_db::DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.set_exchange_active(exchange_id, active)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot_rs.db_upsert_pool_manager(database_path, address, chain, kind,
/// state_view, exchange_id) -> PoolManagerRow`
///
/// Upsert a `pool_managers` row by `(address, chain)` (the get-or-create
/// substrate for the V4 manager row the `exchange activate uniswap_v4` CLI
/// creates). `pool_managers` HAS the unique index on `(address, chain)` so this
/// uses `INSERT … ON CONFLICT … RETURNING` — idempotent: re-calling with a
/// changed `state_view` updates the row in place (same id), re-calling
/// identical leaves it unchanged. `address` / `state_view` are stored
/// EIP-55-checksummed. Raises `ValueError` on a parse or DB failure.
#[pyfunction]
pub(crate) fn db_upsert_pool_manager(
    py: Python<'_>,
    database_path: &str,
    address: &str,
    chain: i64,
    kind: &str,
    state_view: Option<&str>,
    exchange_id: i64,
) -> PyResult<Py<PyPoolManagerRow>> {
    let path = PathBuf::from(database_path);
    let address = Address::parse_checksummed(address, None)
        .map_err(|e| PyValueError::new_err(format!("bad pool manager address: {e}")))?;
    let state_view = state_view
        .map(|sv| {
            Address::parse_checksummed(sv, None)
                .map_err(|e| PyValueError::new_err(format!("bad state_view address: {e}")))
        })
        .transpose()?;
    let row = py.detach(|| {
        let (db, _state) =
            degenbot_db::DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.upsert_pool_manager(address, chain, kind, state_view, exchange_id)
            .map_err(|e| db_err_to_py(&e))
    })?;
    Py::new(py, PyPoolManagerRow::from(row))
}

// ═══════════════════════════════════════════════════════════════════════════
// Module registration
// ═══════════════════════════════════════════════════════════════════════════

/// Register the discovery seam functions + pyclasses on `m`.
///
/// # Errors
///
/// Returns a [`PyErr`] if any `add_function` / `add_class` call fails.
pub fn add_discovery_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(db_upsert_v2_pools, m)?)?;
    m.add_function(wrap_pyfunction!(db_upsert_v3_pools, m)?)?;
    m.add_function(wrap_pyfunction!(db_upsert_v4_pools, m)?)?;
    m.add_function(wrap_pyfunction!(db_set_exchange_last_update_block, m)?)?;
    m.add_function(wrap_pyfunction!(db_upsert_exchange, m)?)?;
    m.add_function(wrap_pyfunction!(db_set_exchange_active, m)?)?;
    m.add_function(wrap_pyfunction!(db_upsert_pool_manager, m)?)?;
    m.add_class::<PyV2PoolRowInput>()?;
    m.add_class::<PyV3PoolRowInput>()?;
    m.add_class::<PyV4PoolRowInput>()?;
    Ok(())
}
