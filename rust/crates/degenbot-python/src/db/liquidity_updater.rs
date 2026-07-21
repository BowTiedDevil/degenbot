//! `PyO3` seam for the V3/V4 DB-aware liquidity updater — §4.3 (task QJSCA5).
//!
//! Wraps `degenbot-db`'s apply-and-persist core
//! ([`degenbot_db::DegenbotDb::apply_v3_liquidity_updates`] /
//! [`apply_v4_liquidity_updates`]) as module-level `#[pyfunction]`s taking a
//! `database_path` (same pattern as `db_create_new_database`). The Python
//! `cli/pool.py::apply_v3/v4_liquidity_updates` shells decode the raw
//! `LogReceipt`s into [`PyLiquidityUpdateEvent`] records + delegate here —
//! the Rust core owns the math (`apply_liquidity_mapping_update`) + the DB
//! persistence (reconstitute map → apply events → upsert positions/init-maps,
//! delete stale, stamp `liquidity_update_block`/`log_index`).
//!
//! `py.detach()` wraps the DB I/O (no GIL held during the apply loop).
//!
//! The `liquidity_delta` is a signed `i128` — V3/V4 deltas fit in the
//! contract's `int128` range (Burn events are negative, Mint positive, V4
//! Modify already signed). `OverflowError` propagates if a caller passes a value
//! outside that range (matches the `cl_apply_liquidity_mapping_update` seam).

use std::path::PathBuf;

use pyo3::prelude::*;

use crate::db::db_err_to_py;

/// One decoded liquidity event the apply loop consumes (QJSCA5 §4.3).
///
/// Mirrors [`degenbot_db::LiquidityUpdateEvent`]: the
/// `(block_number, log_index, tick_lower, tick_upper, liquidity_delta)`
/// tuple. The Python `cli/pool.py` apply shells decode the raw `LogReceipt`
/// (topics/data → tick bounds + signed delta, with V3 Burn negation) +
/// build these records; the Rust core applies them in order.
///
/// `liquidity_delta` is the SIGNED delta — the caller produces it (Burn
/// negated for V3; V4 Modify decoded signed). The block/log-index ordering
/// invariants (`block >= last; same-block log_index > last`) are enforced in
/// the Rust apply loop (panics on violation, matching the Python `assert`s).
#[pyclass(name = "LiquidityUpdateEvent", module = "degenbot._ffi.db")]
pub struct PyLiquidityUpdateEvent {
    block_number: u64,
    log_index: u64,
    tick_lower: i32,
    tick_upper: i32,
    /// Signed delta; stored as i128 (contract int128 range) + widened to I256
    /// at the substrate boundary.
    liquidity_delta: i128,
}

#[pymethods]
impl PyLiquidityUpdateEvent {
    /// Build a liquidity-update event record.
    ///
    /// `liquidity_delta` must fit in i128 (the V3/V4 contract `int128` range);
    /// raises `OverflowError` otherwise.
    #[new]
    #[pyo3(signature = (block_number, log_index, tick_lower, tick_upper, liquidity_delta))]
    fn new(
        block_number: u64,
        log_index: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
    ) -> Self {
        Self {
            block_number,
            log_index,
            tick_lower,
            tick_upper,
            liquidity_delta,
        }
    }
}

impl PyLiquidityUpdateEvent {
    /// Convert to the substrate row type (widening i128 → I256).
    fn to_event(&self) -> degenbot_db::LiquidityUpdateEvent {
        degenbot_db::LiquidityUpdateEvent {
            block_number: self.block_number,
            log_index: self.log_index,
            tick_lower: self.tick_lower,
            tick_upper: self.tick_upper,
            liquidity_delta: alloy::primitives::I256::try_from(self.liquidity_delta)
                .unwrap_or(alloy::primitives::I256::ZERO),
        }
    }
}

/// `degenbot_rs.db_apply_v3_liquidity_updates(database_path, chain_id, pool_address, events) -> bool`
///
/// Apply a sequence of [`PyLiquidityUpdateEvent`]s to the V3 pool identified by
/// `(chain_id, pool_address)`: reconstitute the `LiquidityMap` from DB rows →
/// apply each event (`apply_liquidity_mapping_update`) → upsert live
/// positions/init-maps, delete stale, stamp `liquidity_update_block`/
/// `log_index` with the last event. Returns `False` if the pool row isn't found
/// (mirrors the Python `if pool_in_db is None: return`); `True` after a
/// successful apply. The block/log-index ordering invariant is enforced
/// (panics on violation, matching the Python `assert`s).
#[pyfunction]
pub(crate) fn db_apply_v3_liquidity_updates(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    pool_address: &str,
    events: Vec<Py<PyLiquidityUpdateEvent>>,
) -> PyResult<bool> {
    let path = PathBuf::from(database_path);
    let rust_events: Vec<degenbot_db::LiquidityUpdateEvent> = events
        .into_iter()
        .map(|e| e.borrow(py).to_event())
        .collect();
    py.detach(|| {
        let (db, _state) =
            degenbot_db::DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.apply_v3_liquidity_updates(chain_id, pool_address, &rust_events)
            .map_err(|e| db_err_to_py(&e))
    })
}

/// `degenbot_rs.db_apply_v4_liquidity_updates(database_path, pool_hash_hex, pool_manager_chain, events) -> bool`
///
/// Apply a sequence of [`PyLiquidityUpdateEvent`]s to the V4 pool identified by
/// `(pool_hash, pool_manager_chain)`. Same semantics as
/// [`db_apply_v3_liquidity_updates`] (reconstitute → apply → persist → stamp);
/// differs only in the row lookups (`uniswap_v4_pools` × `managed_pools` ×
/// `pool_managers`) + the tables written (`managed_pool_*`).
#[pyfunction]
pub(crate) fn db_apply_v4_liquidity_updates(
    py: Python<'_>,
    database_path: &str,
    pool_hash_hex: &str,
    pool_manager_chain: i64,
    events: Vec<Py<PyLiquidityUpdateEvent>>,
) -> PyResult<bool> {
    let path = PathBuf::from(database_path);
    let rust_events: Vec<degenbot_db::LiquidityUpdateEvent> = events
        .into_iter()
        .map(|e| e.borrow(py).to_event())
        .collect();
    py.detach(|| {
        let (db, _state) =
            degenbot_db::DegenbotDb::open_for_writes(&path).map_err(|e| db_err_to_py(&e))?;
        db.apply_v4_liquidity_updates(pool_hash_hex, pool_manager_chain, &rust_events)
            .map_err(|e| db_err_to_py(&e))
    })
}
