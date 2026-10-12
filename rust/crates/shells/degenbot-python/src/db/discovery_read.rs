//! `PyO3` seam for the database discovery read (`degenbot_db::discovery_read`).
//!
//! Exposes [`degenbot_db::DegenbotDb::fetch_discovery_rows`] — the typed
//! `DiscoveryPoolRow` enumeration (V2 / V3 / Uniswap V4) the pure-Rust
//! `build_paths.py` analogue reads — to Python as [`PyDiscoveryPoolRow`]
//! values. One DB read answers "every candidate pool on this chain", carrying
//! the base pool row, the two token rows, the owning exchange row, and the
//! per-family detail columns (fees, `tick_spacing`, Aerodrome `stable`, the
//! liquidity-update marker). The Python `degenbot.discovery` module projects
//! the token-pair query from it.
//!
//! READ exposure only: the constructor-free reader the three-layer discovery
//! ladder needs. It is deliberately NOT behind the `writer` feature (ADR-070
//! gates only write-capable constructors). The `database_path` + `chain_id`
//! args and the `py.detach()` DB I/O mirror the sibling read seams
//! ([`crate::db::read_seams::db_fetch_graph_edition`]).

use std::path::{Path, PathBuf};

use pyo3::prelude::*;

use crate::db::db_err_to_py;
use degenbot_db::{DegenbotDb, DiscoveryPoolRow};

/// One discovered candidate pool — the typed `degenbot_db::discovery_read` row
/// flattened across the three family variants so Python sees one uniform view.
///
/// Family-scoped fields are `None` where the family has no such column:
/// `address` is `None` for V4 (`manager` + `pool_hash` identify it instead),
/// `stable` is `None` outside Aerodrome V2, `tick_spacing` is `None` for V2,
/// and `update_block` (the liquidity-update marker) is `None` for V2 (whose
/// subclass tables carry no such column).
#[pyclass(name = "DiscoveryPoolRow", module = "degenbot._ffi.db")]
pub struct PyDiscoveryPoolRow {
    kind: String,
    family: String,
    address: Option<String>,
    manager: Option<String>,
    pool_hash: Option<String>,
    token0: String,
    token1: String,
    token0_id: i64,
    token1_id: i64,
    db_id: i64,
    factory: String,
    fee: Option<i64>,
    tick_spacing: Option<i64>,
    stable: Option<bool>,
    update_block: Option<i64>,
}

impl PyDiscoveryPoolRow {
    /// Flatten one core [`DiscoveryPoolRow`] into the uniform Python view.
    fn from_row(row: &DiscoveryPoolRow) -> Self {
        let kind = row.kind().to_string();
        let token0 = row.token0().address.to_checksum(None);
        let token1 = row.token1().address.to_checksum(None);
        let token0_id = row.token0().id;
        let token1_id = row.token1().id;
        let factory = row.exchange().factory.to_checksum(None);
        match row {
            DiscoveryPoolRow::V2(r) => Self {
                kind,
                family: "V2".to_string(),
                address: Some(r.pool.address.to_checksum(None)),
                manager: None,
                pool_hash: None,
                token0,
                token1,
                token0_id,
                token1_id,
                db_id: r.pool.id,
                factory,
                fee: Some(r.fee_token0),
                tick_spacing: None,
                stable: r.stable,
                update_block: None,
            },
            DiscoveryPoolRow::V3(r) => Self {
                kind,
                family: "V3".to_string(),
                address: Some(r.pool.address.to_checksum(None)),
                manager: None,
                pool_hash: None,
                token0,
                token1,
                token0_id,
                token1_id,
                db_id: r.pool.id,
                factory,
                fee: Some(r.fee_token0),
                tick_spacing: Some(r.tick_spacing),
                stable: None,
                update_block: r.liquidity_update_block,
            },
            DiscoveryPoolRow::V4(r) => Self {
                kind,
                family: "V4".to_string(),
                address: None,
                manager: Some(r.manager.address.to_checksum(None)),
                pool_hash: Some(format!("{:#x}", r.pool_hash)),
                token0,
                token1,
                token0_id,
                token1_id,
                db_id: r.managed_pool_id,
                factory,
                fee: Some(r.fee_currency0),
                tick_spacing: Some(r.tick_spacing),
                stable: None,
                update_block: r.liquidity_update_block,
            },
        }
    }
}

#[pymethods]
impl PyDiscoveryPoolRow {
    /// The raw polymorphic `kind` string (`"uniswap_v3"`, `"aerodrome_v2"`, ...).
    #[getter]
    fn kind(&self) -> String {
        self.kind.clone()
    }

    /// The coarse family tag (`"V2"` / `"V3"` / `"V4"`) — the key Python maps
    /// to `degenbot.pathfinding.PoolKind`.
    #[getter]
    fn family(&self) -> String {
        self.family.clone()
    }

    /// The V2/V3 checksummed pool address; `None` for V4.
    #[getter]
    fn address(&self) -> Option<String> {
        self.address.clone()
    }

    /// The V4 pool-manager address; `None` for V2/V3.
    #[getter]
    fn manager(&self) -> Option<String> {
        self.manager.clone()
    }

    /// The V4 pool hash (`0x`-prefixed hex); `None` for V2/V3.
    #[getter]
    fn pool_hash(&self) -> Option<String> {
        self.pool_hash.clone()
    }

    /// The `token0` / `currency0` checksummed address.
    #[getter]
    fn token0(&self) -> String {
        self.token0.clone()
    }

    /// The `token1` / `currency1` checksummed address.
    #[getter]
    fn token1(&self) -> String {
        self.token1.clone()
    }

    /// The DB token id for `token0`.
    #[getter]
    fn token0_id(&self) -> i64 {
        self.token0_id
    }

    /// The DB token id for `token1`.
    #[getter]
    fn token1_id(&self) -> i64 {
        self.token1_id
    }

    /// The raw DB row id (`pools.id` for V2/V3, `managed_pools.id` for V4).
    #[getter]
    fn db_id(&self) -> i64 {
        self.db_id
    }

    /// The owning exchange's checksummed factory address.
    #[getter]
    fn factory(&self) -> String {
        self.factory.clone()
    }

    /// The pool's fee (`fee_token0` for V2/V3, `fee_currency0` for V4).
    #[getter]
    fn fee(&self) -> Option<i64> {
        self.fee
    }

    /// The V3/V4 tick spacing; `None` for V2.
    #[getter]
    fn tick_spacing(&self) -> Option<i64> {
        self.tick_spacing
    }

    /// The Aerodrome V2 stable flag; `None` for every other family.
    #[getter]
    fn stable(&self) -> Option<bool> {
        self.stable
    }

    /// The liquidity-update marker block (V3/V4); `None` for V2.
    #[getter]
    fn update_block(&self) -> Option<i64> {
        self.update_block
    }
}

/// `degenbot._ffi.db.db_fetch_discovery_rows(database_path, chain_id) -> list[DiscoveryPoolRow]`
///
/// Read every candidate pool for `chain_id` from the Rust `discovery_read`
/// surface — the V2/V3 `UNION ALL` (ascending `pools.id`) then the V4 branch
/// (ascending `managed_pools.id`). The single read that answers a general
/// token-pair discovery query without the graph seam's degree >= 2 cycle
/// filter. Read-only; raises `ValueError` on a DB/query/decode failure after
/// releasing the GIL.
///
/// # Errors
///
/// Returns `ValueError` when the database cannot be opened, queried, or a row
/// fails to decode.
#[pyfunction]
pub(crate) fn db_fetch_discovery_rows(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
) -> PyResult<Vec<Py<PyDiscoveryPoolRow>>> {
    let path = PathBuf::from(database_path);
    let rows = py.detach(|| -> Result<_, PyErr> {
        let (db, _state) = DegenbotDb::open(Path::new(&path)).map_err(|e| db_err_to_py(&e))?;
        db.fetch_discovery_rows(chain_id)
            .map_err(|e| db_err_to_py(&e))
    })?;
    rows.iter()
        .map(|row| Py::new(py, PyDiscoveryPoolRow::from_row(row)))
        .collect()
}
