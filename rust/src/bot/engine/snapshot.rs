//! `PyO3` wrapper for the `UniswapEngine` — snapshot `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` (ergo UG6FKN task 74W2Z6),
//! mirroring `crates/degenbot-bot/src/optimizers/uniswap_engine/`'s per-concern
//! layout. PyO3 allows multiple `#[pymethods] impl PyUniswapArbEngine { … }`
//! blocks per type, so each concern file contributes one slice.

use super::*;
use crate::prelude::*;

use super::verify::map_verify_err;

#[pymethods]
impl PyUniswapArbEngine {
    /// Load a V3 liquidity snapshot from a binary buffer.
    ///
    /// The binary format is documented in `snapshot_binary.py`:
    /// ```text
    /// [1 byte: version] [4 bytes LE: pool_count]
    /// Per pool:
    ///   [20 bytes: pool address]
    ///   [4 bytes LE: tick_count]
    ///   Per tick:
    ///     [4 bytes LE: tick_index (i32)]
    ///     [16 bytes LE: liquidity_gross (u128)]
    ///     [16 bytes LE: liquidity_net (i128)]
    /// ```
    ///
    /// Requires `Subscribed` or `SnapshotLoaded` phase.
    /// Raises `RuntimeError` if V3 snapshot already loaded.
    #[allow(clippy::needless_pass_by_value)]
    fn load_v3_snapshot(&self, data: Vec<u8>) -> PyResult<()> {
        let phase = self.current_phase();
        // Allow loading from Created (unit tests) or Subscribed/SnapshotLoaded (production)
        phase
            .require_before(EnginePhase::Resumed, "load_v3_snapshot")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        if self.v3_snapshot.is_loaded() {
            let msg = "Cannot load V3 snapshot: already loaded. Call clear_v3_snapshot() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let snapshot = Self::deserialize_v3_snapshot(&data)?;
        self.v3_snapshot.load(snapshot);

        // Advance phase to SnapshotLoaded (if not already there from V4)
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }

        Ok(())
    }

    /// Load a V4 liquidity snapshot from a binary buffer.
    ///
    /// The binary format is documented in `snapshot_binary.py`:
    /// ```text
    /// [1 byte: version] [4 bytes LE: pool_manager_count]
    /// Per pool_manager:
    ///   [20 bytes: pool_manager address]
    ///   [4 bytes LE: pool_id_count]
    ///   Per pool_id:
    ///     [32 bytes: pool_id]
    ///     [4 bytes LE: tick_count]
    ///     Per tick:
    ///       [4 bytes LE: tick_index (i32)]
    ///       [16 bytes LE: liquidity_gross (u128)]
    ///       [16 bytes LE: liquidity_net (i128)]
    /// ```
    ///
    /// Requires `Subscribed` or `SnapshotLoaded` phase.
    /// Raises `RuntimeError` if V4 snapshot already loaded.
    #[allow(clippy::needless_pass_by_value)]
    fn load_v4_snapshot(&self, data: Vec<u8>) -> PyResult<()> {
        let phase = self.current_phase();
        phase
            .require_before(EnginePhase::Resumed, "load_v4_snapshot")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        if self.v4_snapshot.is_loaded() {
            let msg = "Cannot load V4 snapshot: already loaded. Call clear_v4_snapshot() first.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }

        let snapshot = Self::deserialize_v4_snapshot(&data)?;
        self.v4_snapshot.load(snapshot);

        // Advance phase to SnapshotLoaded (if not already there from V3)
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }

        Ok(())
    }

    /// Drop the stored V3 snapshot, freeing memory.
    /// Idempotent — no-op if no V3 snapshot is loaded.
    fn clear_v3_snapshot(&self) {
        self.v3_snapshot.clear();
    }

    /// Drop the stored V4 snapshot, freeing memory.
    /// Idempotent — no-op if no V4 snapshot is loaded.
    fn clear_v4_snapshot(&self) {
        self.v4_snapshot.clear();
    }

    /// Begin streaming V3 snapshot data into the engine, one pool at a time.
    ///
    /// Call `insert_v3_pool_snapshot` for each pool, then `finish_v3_snapshot`
    /// to finalize. This avoids building the entire snapshot dict in memory.
    ///
    /// Can be called in Created or Subscribed phase. Idempotent — calling again
    /// while a stream is in progress is a no-op.
    fn begin_v3_snapshot_stream(&self) -> PyResult<()> {
        let phase = self.current_phase();
        phase
            .require_before(EnginePhase::Resumed, "begin_v3_snapshot_stream")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        if self.v3_snapshot.is_loaded() {
            let msg = "Cannot begin V3 snapshot stream: snapshot already loaded.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        self.v3_snapshot.begin_load();
        Ok(())
    }

    /// Insert a single V3 pool's tick data into the in-progress snapshot stream.
    ///
    /// Args:
    ///     `pool_address`: Hex string of the pool address.
    ///     `tick_data`: Dict mapping `tick_index` (int) → (`liquidity_gross`, `liquidity_net`) tuple.
    fn insert_v3_pool_snapshot(
        &self,
        pool_address: &str,
        tick_data: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<()> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;

        let mut rust_tick_data = HashMap::new();
        for (py_tick, py_values) in tick_data.iter() {
            let tick_index: i32 = py_tick.extract()?;
            let values: (u128, i128) = py_values.extract()?;
            rust_tick_data.insert(tick_index, make_tick_info(values.0, values.1));
        }

        map_verify_err(self.v3_snapshot.insert(addr, rust_tick_data))
    }

    /// Finalize the V3 snapshot stream and transition to `SnapshotLoaded` phase.
    fn finish_v3_snapshot(&self) -> PyResult<()> {
        let phase = self.current_phase();
        if !self.v3_snapshot.is_loaded() {
            let msg = "No V3 snapshot stream in progress.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
    }

    /// Begin streaming V4 snapshot data into the engine, one pool at a time.
    fn begin_v4_snapshot_stream(&self) -> PyResult<()> {
        let phase = self.current_phase();
        phase
            .require_before(EnginePhase::Resumed, "begin_v4_snapshot_stream")
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;

        if self.v4_snapshot.is_loaded() {
            let msg = "Cannot begin V4 snapshot stream: snapshot already loaded.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        self.v4_snapshot.begin_load();
        Ok(())
    }

    /// Insert a single V4 pool's tick data into the in-progress snapshot stream.
    ///
    /// Args:
    ///     `pool_manager`: Hex string of the pool manager address.
    ///     `pool_id_hex`: Hex string of the 32-byte pool ID.
    ///     `tick_data`: Dict mapping `tick_index` (int) → (`liquidity_gross`, `liquidity_net`) tuple.
    fn insert_v4_pool_snapshot(
        &self,
        pool_manager: &str,
        pool_id_hex: &str,
        tick_data: &Bound<'_, pyo3::types::PyDict>,
    ) -> PyResult<()> {
        let pm_addr = pool_manager.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
        })?;
        let pool_id = degenbot_core::hex_utils::decode_32byte_hex(pool_id_hex)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

        let mut rust_tick_data = HashMap::new();
        for (py_tick, py_values) in tick_data.iter() {
            let tick_index: i32 = py_tick.extract()?;
            let values: (u128, i128) = py_values.extract()?;
            rust_tick_data.insert(tick_index, make_tick_info(values.0, values.1));
        }

        map_verify_err(self.v4_snapshot.insert((pm_addr, pool_id), rust_tick_data))
    }

    /// Finalize the V4 snapshot stream and transition to `SnapshotLoaded` phase.
    fn finish_v4_snapshot(&self) -> PyResult<()> {
        let phase = self.current_phase();
        if !self.v4_snapshot.is_loaded() {
            let msg = "No V4 snapshot stream in progress.";
            return Err(pyo3::exceptions::PyRuntimeError::new_err(msg));
        }
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
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

        if self.v3_snapshot.is_loaded() {
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

        self.v3_snapshot.load(result);
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

        if self.v4_snapshot.is_loaded() {
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

        self.v4_snapshot.load(result);
        if phase < EnginePhase::SnapshotLoaded {
            self.set_phase(EnginePhase::SnapshotLoaded);
        }
        Ok(())
    }
}
