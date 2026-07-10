//! `PyO3` seam for the V3/V4 snapshot DB readers — a thin `#[pyclass]`
//! `PyDatabaseSnapshot` holding a [`DegenbotDb`] read handle, exposing the
//! SLHSM4 snapshot read fns (`fetch_liquidity_map` / `fetch_liquidity_map_v4`
//! / `fetch_all_liquidity_maps` / `fetch_newest_update_block` /
//! `fetch_v3_pool_addresses` / `fetch_v4_pool_hashes`) to Python.
//!
//! The core owns the reads; these extract args, release the GIL via
//! `py.detach()` for the `SQLite` I/O, call core, then wrap results into Python
//! `LiquidityMap` / `BitmapAtWord` / `LiquidityAtTick` shaped dicts + flat
//! `get_all_liquidity_maps` dicts + `get_pools` sets, matching the existing
//! return shapes so the Python `DatabaseSnapshot` callers are unchanged. No
//! business logic (three-layer architecture, ADR-005).
//!
//! The Python `src/degenbot/uniswap/{v3,v4}_snapshot.py::DatabaseSnapshot`
//! classes are converted to delegating shells over this seam.

use std::collections::HashMap;
use std::path::Path;

use alloy::primitives::{Address, B256, U256};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PySet, PyString, PyTuple};
use std::str::FromStr;

use crate::conversion::alloy::u256_to_py;
use crate::db::db_err_to_py;
use degenbot_db::snapshot::{LiquidityMap, PoolKey};
use degenbot_db::{DegenbotDb, ExchangeFamily};

/// `degenbot_rs.PyDatabaseSnapshot` — a read-only V3/V4 snapshot handle over a
/// degenbot `SQLite` DB file. Opens its own connection (WAL, `query_only=on`)
/// from `database_path`; the Python `DatabaseSnapshot` shell constructs one
/// per chain + delegates every read to it.
#[pyclass(name = "PyDatabaseSnapshot")]
pub struct PyDatabaseSnapshot {
    db: DegenbotDb,
    chain_id: i64,
}

#[pymethods]
impl PyDatabaseSnapshot {
    /// `PyDatabaseSnapshot(chain_id, database_path)`.
    ///
    /// Opens the DB read-only (matching the Python `DatabaseSnapshot` which
    /// operates over a session). Raises `ValueError` if the DB cannot be
    /// opened or has an unrecognized schema.
    #[new]
    #[pyo3(signature = (chain_id, database_path))]
    fn new(chain_id: i64, database_path: &str) -> PyResult<Self> {
        let (db, _state) =
            DegenbotDb::open(Path::new(database_path)).map_err(|e| db_err_to_py(&e))?;
        Ok(Self { db, chain_id })
    }

    /// V3 `get_liquidity_map(pool_address)` → `LiquidityMap`-shaped dict or
    /// `None`. Returns `{"tick_bitmap": {word: {"bitmap": int}},
    /// "tick_data": {tick: {"liquidity_gross": int, "liquidity_net": int}}}`.
    fn get_liquidity_map_v3<'py>(
        &self,
        py: Python<'py>,
        pool_address: &str,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        let addr = Address::parse_checksummed(pool_address, None)
            .map_err(|e| PyValueError::new_err(format!("bad pool address: {e}")))?;
        let map = py
            .detach(|| self.db.fetch_liquidity_map(addr))
            .map_err(|e| db_err_to_py(&e))?;
        map.map(|m| build_liquidity_map_py(py, &m)).transpose()
    }

    /// V4 `get_liquidity_map(pool_manager, pool_id)` → `LiquidityMap`-shaped
    /// dict or `None`. `pool_id` may be `bytes` or a 0x-hex `str` (the pool
    /// hash).
    fn get_liquidity_map_v4<'py>(
        &self,
        py: Python<'py>,
        pool_manager: &str,
        pool_id: &Bound<'_, PyAny>,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        let mgr = Address::parse_checksummed(pool_manager, None)
            .map_err(|e| PyValueError::new_err(format!("bad pool manager address: {e}")))?;
        let hash = extract_pool_hash(pool_id)?;
        let map = py
            .detach(|| self.db.fetch_liquidity_map_v4(mgr, hash))
            .map_err(|e| db_err_to_py(&e))?;
        map.map(|m| build_liquidity_map_py(py, &m)).transpose()
    }

    /// V3 `get_all_liquidity_maps()` → `{pool_address: {tick: (gross, net)}}`.
    fn get_all_liquidity_maps_v3<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let rows = py
            .detach(|| {
                self.db
                    .fetch_all_liquidity_maps(self.chain_id, ExchangeFamily::V3)
            })
            .map_err(|e| db_err_to_py(&e))?;
        build_all_liquidity_maps_py(py, rows)
    }

    /// V4 `get_all_liquidity_maps()` →
    /// `{(pool_manager, pool_hash): {tick: (gross, net)}}`.
    fn get_all_liquidity_maps_v4<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let rows = py
            .detach(|| {
                self.db
                    .fetch_all_liquidity_maps(self.chain_id, ExchangeFamily::V4)
            })
            .map_err(|e| db_err_to_py(&e))?;
        build_all_liquidity_maps_py(py, rows)
    }

    /// V3 `get_newest_block()` → newest V3 exchange `last_update_block` or
    /// `None` (any NULL → `None`, mirroring the Python oracle).
    fn get_newest_block_v3(&self, py: Python<'_>) -> PyResult<Option<i64>> {
        py.detach(|| {
            self.db
                .fetch_newest_update_block(self.chain_id, ExchangeFamily::V3)
        })
        .map_err(|e| db_err_to_py(&e))
    }

    /// V4 `get_newest_block()` → newest V4 exchange `last_update_block` or
    /// `None`.
    fn get_newest_block_v4(&self, py: Python<'_>) -> PyResult<Option<i64>> {
        py.detach(|| {
            self.db
                .fetch_newest_update_block(self.chain_id, ExchangeFamily::V4)
        })
        .map_err(|e| db_err_to_py(&e))
    }

    /// V3 `get_pools()` → set of V3 pool addresses.
    fn get_pools_v3<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PySet>> {
        let addrs = py
            .detach(|| self.db.fetch_v3_pool_addresses(self.chain_id))
            .map_err(|e| db_err_to_py(&e))?;
        let set = PySet::empty(py)?;
        for a in addrs {
            set.add(PyString::new(py, &a.to_checksum(None)))?;
        }
        Ok(set)
    }

    /// V4 `get_pools()` → set of V4 `pool_hash` hex strings.
    fn get_pools_v4<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PySet>> {
        let hashes = py
            .detach(|| self.db.fetch_v4_pool_hashes(self.chain_id))
            .map_err(|e| db_err_to_py(&e))?;
        let set = PySet::empty(py)?;
        for (h, _addr) in hashes {
            set.add(PyString::new(py, &h))?;
        }
        Ok(set)
    }
}

/// Extract a V4 `pool_hash` `B256` from a Python `bytes` or 0x-hex `str`.
fn extract_pool_hash(obj: &Bound<'_, PyAny>) -> PyResult<B256> {
    if let Ok(bytes) = obj.extract::<Vec<u8>>() {
        if bytes.len() == 32 {
            return Ok(B256::from_slice(&bytes));
        }
        return Err(PyValueError::new_err(format!(
            "pool_id bytes must be length 32, got {}",
            bytes.len()
        )));
    }
    if let Ok(s) = obj.extract::<String>() {
        return B256::from_str(&s)
            .map_err(|e| PyValueError::new_err(format!("bad pool_id hex: {e}")));
    }
    Err(PyValueError::new_err("pool_id must be bytes or 0x-hex str"))
}

/// Build the `LiquidityMap`-shaped Python dict from a Rust [`LiquidityMap`].
fn build_liquidity_map_py<'py>(
    py: Python<'py>,
    map: &LiquidityMap,
) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);

    let bitmap = PyDict::new(py);
    for (word, entry) in &map.tick_bitmap {
        let inner = PyDict::new(py);
        inner.set_item("bitmap", u256_to_py(py, &entry.bitmap)?)?;
        bitmap.set_item(*word, inner)?;
    }
    out.set_item("tick_bitmap", bitmap)?;

    let data = PyDict::new(py);
    for (tick, entry) in &map.tick_data {
        let inner = PyDict::new(py);
        inner.set_item("liquidity_gross", u256_to_py(py, &entry.liquidity_gross)?)?;
        inner.set_item("liquidity_net", u256_to_py(py, &entry.liquidity_net)?)?;
        data.set_item(*tick, inner)?;
    }
    out.set_item("tick_data", data)?;

    Ok(out)
}

/// Build the flat `get_all_liquidity_maps` Python dict — V3 keyed by address
/// string, V4 keyed by `(address, pool_hash)` tuple (dispatched on the
/// [`PoolKey`] variant inside each row).
type TickRows = Vec<(PoolKey, HashMap<i32, (U256, U256)>)>;

fn build_all_liquidity_maps_py(py: Python<'_>, rows: TickRows) -> PyResult<Bound<'_, PyDict>> {
    let out = PyDict::new(py);
    for (key, ticks) in rows {
        let tick_dict = PyDict::new(py);
        for (tick, (gross, net)) in ticks {
            let pair = PyTuple::new(py, [u256_to_py(py, &gross)?, u256_to_py(py, &net)?])?;
            tick_dict.set_item(tick, pair)?;
        }
        match key {
            PoolKey::V3(addr) => {
                out.set_item(addr.to_checksum(None), tick_dict)?;
            }
            PoolKey::V4 {
                pool_manager,
                pool_hash,
            } => {
                let k = PyTuple::new(
                    py,
                    [
                        PyString::new(py, &pool_manager.to_checksum(None)).into_any(),
                        PyString::new(py, &pool_hash).into_any(),
                    ],
                )?;
                out.set_item(k, tick_dict)?;
            }
        }
    }
    Ok(out)
}
