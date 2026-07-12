//! `PyO3` wrapper for the `UniswapEngine` — snapshot `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` (ergo UG6FKN task 74W2Z6),
//! mirroring `crates/degenbot-bot/src/solvers/uniswap_engine/`'s per-concern
//! layout. `PyO3` allows multiple `#[pymethods] impl PyUniswapArbEngine { … }`
//! blocks per type, so each concern file contributes one slice.
//!
//! RUQ637: the `SnapshotStore` fields were moved OFF `PyUniswapArbEngine` into
//! core `BotState` (so the store is the single source of snapshot tick data,
//! consumed at registration via `take`). These `#[pymethods]` are the thin
//! Python-facing ingestion surface — `load_*_from_py` crosses `PyO3` ONCE per
//! family with the whole dict (the non-DB path), delegating to the core store
//! through the shared `Arc<RwLock<BotState>>` (`engine.core`) — pure arg
//! extraction + store write, no business logic. B3OROH's
//! `Bot::load_snapshot_from_db` bypasses this surface entirely (writes the
//! core store from Rust at `PyBot` construction — zero tick dicts cross `PyO3`).
//!
//! DADWUP: the per-pool ingestion surface (`begin_*_snapshot_stream` /
//! `insert_*_pool_snapshot` / `finish_*_snapshot`) + the `SQLAlchemy` `yield_per`
//! loops that drove it (`stream_*_to_engine`) are retired — the DB path is
//! Rust-owned, the non-DB path crosses once with the whole dict. No per-pool
//! crossings remain.

use super::{make_tick_info, Address, EnginePhase, HashMap, PyUniswapArbEngine};
use crate::prelude::*;

use degenbot_bot::bot_core::snapshot_verify::SnapshotStore;

impl PyUniswapArbEngine {
    /// Acquire the engine + core write lock, returning a guard over the core
    /// `SnapshotStore<Address>`. Held briefly under one engine-lock acquisition
    /// so the pump cannot interleave a registration `take` mid-load.
    fn with_v3_store<R>(
        &self,
        f: impl FnOnce(&degenbot_bot::bot_core::snapshot_verify::SnapshotStore<Address>) -> R,
    ) -> R {
        let engine = self.engine.lock();
        let core = engine.core.write();
        f(core.v3_snapshot_store())
    }

    /// V4 twin of [`Self::with_v3_store`].
    fn with_v4_store<R>(
        &self,
        f: impl FnOnce(
            &degenbot_bot::bot_core::snapshot_verify::SnapshotStore<(
                Address,
                degenbot_decoders::v4_swap_decoder::PoolId,
            )>,
        ) -> R,
    ) -> R {
        let engine = self.engine.lock();
        let core = engine.core.write();
        f(core.v4_snapshot_store())
    }
}

// DADWUP: the per-pool ingestion surface (begin_v3_snapshot_stream /
// insert_v3_pool_snapshot / finish_v3_snapshot + the V4 twins) is retired.
// The DB path loads the whole snapshot inside `Bot::load_snapshot_from_db`
// (B3OROH) at `PyBot` construction — zero tick dicts cross PyO3. The non-DB
// path crosses ONCE per family via `load_v3_snapshot_from_py` /
// `load_v4_snapshot_from_py` with the whole dict. No per-pool crossings
// remain on the Python-facing surface.

#[pymethods]
impl PyUniswapArbEngine {
    /// Drop the stored V3 snapshot, freeing memory.
    /// Idempotent — no-op if no V3 snapshot is loaded.
    fn clear_v3_snapshot(&self) {
        self.with_v3_store(SnapshotStore::clear);
    }

    /// Drop the stored V4 snapshot, freeing memory.
    /// Idempotent — no-op if no V4 snapshot is loaded.
    fn clear_v4_snapshot(&self) {
        self.with_v4_store(SnapshotStore::clear);
    }

    /// Load a V3 liquidity snapshot from a Python dict.
    ///
    /// The dict maps pool address (hex string) → tick data dict,
    /// where tick data maps `tick_index` (int) → (`liquidity_gross`, `liquidity_net`) tuple.
    ///
    /// This is the fast path — no intermediate binary serialization in Python.
    /// The Rust side iterates the `PyO3` dict and builds the internal `HashMap` directly.
    fn load_v3_snapshot_from_py(&self, py_data: &Bound<'_, pyo3::types::PyDict>) -> PyResult<()> {
        let phase = self.current_phase();
        phase
            .require_before(EnginePhase::Resumed, "load_v3_snapshot_from_py")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        if self.with_v3_store(SnapshotStore::is_loaded) {
            let msg = "Cannot load V3 snapshot: already loaded. Call clear_v3_snapshot() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let mut result = HashMap::new();
        for (py_addr, py_tick_dict) in py_data.iter() {
            let addr_str: String = py_addr.extract()?;
            let address = addr_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
            })?;

            let tick_dict = py_tick_dict
                .cast::<pyo3::types::PyDict>()
                .map_err(|_| pyo3::exceptions::PyTypeError::new_err("tick_data must be a dict"))?;

            let mut tick_data = HashMap::new();
            for (py_tick, py_values) in tick_dict.iter() {
                let tick_index: i32 = py_tick.extract()?;
                let values: (u128, i128) = py_values.extract()?;
                tick_data.insert(tick_index, make_tick_info(values.0, values.1));
            }
            result.insert(address, tick_data);
        }

        self.with_v3_store(|store| store.load(result));
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
    }

    /// Load a V4 liquidity snapshot from a Python dict.
    ///
    /// The dict maps `pool_manager` address (hex) → inner dict,
    /// where inner dict maps `pool_id` (hex) → tick data dict,
    /// and tick data maps `tick_index` (int) → (`liquidity_gross`, `liquidity_net`) tuple.
    fn load_v4_snapshot_from_py(&self, py_data: &Bound<'_, pyo3::types::PyDict>) -> PyResult<()> {
        let phase = self.current_phase();
        phase
            .require_before(EnginePhase::Resumed, "load_v4_snapshot_from_py")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        if self.with_v4_store(SnapshotStore::is_loaded) {
            let msg = "Cannot load V4 snapshot: already loaded. Call clear_v4_snapshot() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let mut result = HashMap::new();
        for (py_pm, py_pool_dict) in py_data.iter() {
            let pm_str: String = py_pm.extract()?;
            let pm_address = pm_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "Invalid pool_manager address: {e}"
                ))
            })?;

            let pool_dict = py_pool_dict.cast::<pyo3::types::PyDict>().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err("pool_manager value must be a dict")
            })?;

            for (py_pool_id, py_tick_dict) in pool_dict.iter() {
                let pool_id_hex: String = py_pool_id.extract()?;
                let pool_id = degenbot_core::hex_utils::decode_32byte_hex(&pool_id_hex)
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

                let tick_dict = py_tick_dict.cast::<pyo3::types::PyDict>().map_err(|_| {
                    pyo3::exceptions::PyTypeError::new_err("tick_data must be a dict")
                })?;

                let mut tick_data = HashMap::new();
                for (py_tick, py_values) in tick_dict.iter() {
                    let tick_index: i32 = py_tick.extract()?;
                    let values: (u128, i128) = py_values.extract()?;
                    tick_data.insert(tick_index, make_tick_info(values.0, values.1));
                }
                result.insert((pm_address, pool_id), tick_data);
            }
        }

        self.with_v4_store(|store| store.load(result));
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
    }
}
