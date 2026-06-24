//! `PyO3` wrapper for the `UniswapEngine` — solve `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` (ergo UG6FKN task 74W2Z6),
//! mirroring `crates/degenbot-bot/src/solvers/uniswap_engine/`'s per-concern
//! layout. `PyO3` allows multiple `#[pymethods] impl PyUniswapArbEngine { … }`
//! blocks per type, so each concern file contributes one slice.

use super::{
    hex_string_to_pool_id, make_tick_info, Address, BlockMetadata, DrainSink, HashMap, PyList,
    PyUniswapArbEngine, V4StateSync,
};
use crate::prelude::*;

#[pymethods]
impl PyUniswapArbEngine {
    /// Last block number processed by the pump's drain phase. Routes through
    /// the `SolveCoordinator` (ADR-006 slice 6), not the raw engine: the
    /// coordinator takes its `drain_lock` and returns a drain-consistent
    /// "good" block the whole system has fully drained to — **blocking** any
    /// Python poll until an in-flight drain completes (no Rust/Python race over
    /// a mid-drain cursor read).
    ///
    /// Returns `None` if no block has been processed yet (before the first
    /// `on_drain` / before `start`).
    fn last_processed_block(&self) -> Option<u64> {
        self.coordinator.last_processed_block()
    }

    /// Set the last processed block manually after Python backfill.
    #[pyo3(signature = (block))]
    fn set_last_processed_block(&self, block: u64) {
        self.engine.lock().set_last_processed_block(block);
    }

    /// Resolve and solve all registered paths.
    ///
    /// Called to populate results for the first time (replaces the
    /// removed `freeze()` + `initial_solve()`). Subsequent `process_logs`
    /// calls use dependency tracking to only re-solve affected paths.
    fn solve_all_paths(&self, block_number: u64) {
        self.engine.lock().solve_all_paths(block_number);
    }

    /// Set the maximum age (in blocks) for buffered liquidity events.
    ///
    /// Applies to V3 and V4 sub-engine buffers. Pass `None` for unbounded
    /// (no automatic expiry). Events older than `current_block - max_age`
    /// are expired during `process_block`.
    #[pyo3(signature = (max_age))]
    fn set_event_buffer_max_age(&self, max_age: Option<u64>) {
        self.engine.lock().set_event_buffer_max_age(max_age);
    }

    /// Discard all buffered liquidity events for all unregistered pools.
    fn flush_event_buffer(&self) {
        self.engine.lock().flush_event_buffer();
    }

    /// Number of registered V2 pools.
    fn v2_pool_count(&self) -> usize {
        self.engine.lock().v2_pool_count()
    }

    /// Number of registered V3 pools.
    fn v3_pool_count(&self) -> usize {
        self.engine.lock().v3_pool_count()
    }

    /// Number of registered V4 pools.
    fn v4_pool_count(&self) -> usize {
        self.engine.lock().v4_pool_count()
    }

    /// Apply all buffered **backfill** V3 Mint/Burn events for a pool address
    /// on top of the snapshot-seeded tick data.
    ///
    /// Under the shared-BotState design (ADR-006) V3 pools enter `BotState` via
    /// the bot builders (`py_bot.register_v3_pool` + `update_tick_data`),
    /// NOT via the engine's verify-gated `register_v3_pool`. That builder path
    /// seeds the snapshot tick map but never drains the backfill/pump buffer —
    /// so Mint/Burn events that fired during `backfill_from_snapshot` (before
    /// the pool was registered) sit undrained, leaving phantom/missing
    /// liquidity that fails on-chain verification. This drains both the
    /// backfill and the pump buffer in order (backfill first, then pump),
    /// matching what the orphaned `register_v3_pool` path did via
    /// `register_with_cl_buffers`. Safe to call on an already-registered pool;
    /// a no-op if the pool is unregistered or has no buffered events.
    #[pyo3(signature = (pool_address))]
    fn apply_buffer_v3(&self, pool_address: &str) -> PyResult<()> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let engine = self.engine.lock();
        engine.core.write().apply_backfill_buffer_v3(&addr);
        engine.core.write().apply_pump_buffer_v3(&addr);
        Ok(())
    }

    /// Apply all buffered **backfill** V4 `ModifyLiquidity` events for a pool
    /// on top of the snapshot-seeded tick data. V4 analogue of
    /// [`apply_buffer_v3`][Self::apply_buffer_v3].
    #[pyo3(signature = (pool_manager, pool_id_hex))]
    fn apply_buffer_v4(&self, pool_manager: &str, pool_id_hex: &str) -> PyResult<()> {
        let pm = pool_manager.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
        })?;
        let pool_id = crate::bot::engine::hex_string_to_pool_id(pool_id_hex)?;
        let engine = self.engine.lock();
        engine.core.write().apply_backfill_buffer_v4(pm, pool_id);
        engine.core.write().apply_pump_buffer_v4(pm, pool_id);
        Ok(())
    }

    /// Debug/test seam: buffer a V3 backfill liquidity update (Mint/Burn) for
    /// a pool address WITHOUT applying it. If the pool is already registered it
    /// is applied directly (mirroring `BotState::buffer_backfill_v3_liquidity_update`);
    /// otherwise it is buffered for a later [`apply_buffer_v3`] drain. Used to
    /// test `apply_buffer_v3` against a primed buffer without an RPC backfill.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (pool_address, tick_lower, tick_upper, liquidity_delta, block_number))]
    fn debug_buffer_v3_liquidity_update(
        &self,
        pool_address: &str,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> PyResult<()> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        self.engine
            .lock()
            .core
            .write()
            .buffer_backfill_v3_liquidity_update(
                addr,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            );
        Ok(())
    }

    /// Debug: return the number of buffered liquidity events for a V3 pool address.
    fn debug_v3_buffer_count(&self, pool_address: &str) -> PyResult<usize> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let engine = self.engine.lock();
        let count = engine.core.read().buffered_v3_event_count(&addr);
        Ok(count)
    }

    /// Debug: return the engine's tick data for a V3 pool address as a Python dict.
    /// Maps `tick_index` (int) → (`liquidity_gross`: int, `liquidity_net`: int) tuple.
    /// Returns None if the pool is not registered.
    fn debug_v3_tick_data<'py>(
        &self,
        py: Python<'py>,
        pool_address: &str,
    ) -> PyResult<Option<Bound<'py, pyo3::types::PyDict>>> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let tick_data = {
            let engine = self.engine.lock();
            let core = engine.core.read();
            let Some(key) = core.pool_id_by_address(&addr) else {
                return Ok(None);
            };
            let Some(pool) = core.get_v3_pool(key) else {
                return Ok(None);
            };
            pool.tick_data.clone()
        };

        let dict = pyo3::types::PyDict::new(py);
        for (&tick_idx, info) in &tick_data {
            let lg = info.liquidity_gross.to::<u128>();
            let ln: i128 = info.liquidity_net.try_into().unwrap_or(0i128);
            dict.set_item(tick_idx, (lg, ln))?;
        }
        Ok(Some(dict))
    }

    /// Number of registered paths.
    fn path_count(&self) -> usize {
        self.engine.lock().path_count()
    }

    /// Snapshot the engine-owned state for every hop in a registered path.
    ///
    /// This is a diagnostic helper for investigating simulation failures.
    /// Engine state is captured while the engine lock is held; the lock is
    /// released before any RPC calls. The `rpc_url` argument is accepted for
    /// forward compatibility with on-chain comparison (Slice 3) but is
    /// currently ignored.
    ///
    /// Returns a Python `dict` containing `path_id`, `path_type`, `solve_block`,
    /// and a `hops` list with per-hop engine state.
    ///
    /// Raises `KeyError` if `path_id` is not registered.
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (path_id, rpc_url=None))]
    fn diagnostic_inspect_path(
        &self,
        py: Python<'_>,
        path_id: u64,
        rpc_url: Option<String>,
    ) -> PyResult<pyo3::Py<pyo3::PyAny>> {
        // 1. Snapshot engine-owned state under the lock.
        let engine = self.engine.lock();
        let snapshot = engine.diagnostic_path_state(path_id);
        drop(engine);

        let Some(mut snapshot) = snapshot else {
            return Err(pyo3::exceptions::PyKeyError::new_err(format!(
                "path_id {path_id} is not registered"
            )));
        };

        // 2. Optionally fetch on-chain state and compute diffs.
        let rpc_url = rpc_url.or_else(|| self.verify_rpc_url.lock().clone());
        if let Some(rpc_url) = rpc_url {
            let runtime = degenbot_core::runtime::get_runtime();
            let state_view = *self.verify_state_view.lock();

            match runtime.block_on(degenbot_rpc::provider::AlloyProvider::new(&rpc_url, 3)) {
                Ok(provider) => {
                    if let Err(e) = runtime.block_on(snapshot.fetch_onchain(&provider, state_view))
                    {
                        eprintln!(
                            "[diagnostic_inspect_path] on-chain fetch failed for path {path_id}: {e}"
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[diagnostic_inspect_path] failed to create provider for path {path_id}: {e}"
                    );
                }
            }
        }

        // 3. Convert the snapshot to a Python dict via JSON round-trip.
        let json = serde_json::to_string(&snapshot).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "diagnostic_inspect_path: serialization failed: {e}"
            ))
        })?;

        let json_module = py.import("json").map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "diagnostic_inspect_path: failed to import json: {e}"
            ))
        })?;
        let dict = json_module.getattr("loads")?.call1((json,))?;
        Ok(dict.unbind())
    }

    /// Full-sync V3 pool `tick_data` from Python backfill.
    ///
    /// Unlike `process_logs` (which only inserts `tick_priors`), this method
    /// **replaces** the entire `tick_data` map. This ensures that ticks removed
    /// from Python (because `liquidityGross` went to zero after a Burn) are
    /// also removed from the Rust engine.
    ///
    /// `v3_sync_updates`: list of (`address_str`, `sqrt_price_x96`, liquidity, tick, `tick_data`)
    ///   where `tick_data` is a dict of {`tick_index`: (`liquidity_gross`, `liquidity_net`)}
    #[pyo3(signature = (v3_sync_updates, block_number))]
    fn sync_v3_pool_states(
        &self,
        v3_sync_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        let engine = self.engine.lock();
        let mut core = engine.core.write();
        for item in v3_sync_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 5 {
                let msg = format!(
                    "Expected 5-tuple (address, sqrt_price, liquidity, tick, tick_data), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let addr_str: String = tuple.get_item(0)?.extract()?;
            let addr: Address = addr_str.parse().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
            })?;
            let sqrt_price = crate::conversion::alloy::extract_python_u256(&tuple.get_item(1)?)?;
            let liquidity: u128 = tuple.get_item(2)?.extract()?;
            let tick: i32 = tuple.get_item(3)?.extract()?;

            let td_obj = tuple.get_item(4)?;
            let td_dict = td_obj.cast::<pyo3::types::PyDict>()?;
            let mut rust_tick_data = HashMap::new();
            for (key, value) in td_dict.iter() {
                let tick_idx: i32 = key.extract()?;
                let info_tuple = value.cast::<pyo3::types::PyTuple>()?;
                if info_tuple.len() != 2 {
                    let msg = format!(
                        "Expected 2-tuple (liquidity_gross, liquidity_net), got {} elements",
                        info_tuple.len()
                    );
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let liquidity_gross: u128 = info_tuple.get_item(0)?.extract()?;
                let liquidity_net: i128 = info_tuple.get_item(1)?.extract()?;
                rust_tick_data.insert(tick_idx, make_tick_info(liquidity_gross, liquidity_net));
            }

            core.sync_v3_pool_state(
                addr,
                sqrt_price,
                liquidity,
                tick,
                rust_tick_data,
                block_number,
            );
        }
        Ok(())
    }

    /// Full-sync V4 pool `tick_data` from Python backfill.
    ///
    /// Replaces the entire `tick_data` map. See `sync_v3_pool_states` for rationale.
    ///
    /// `v4_sync_updates`: list of (`pool_manager_str`, `pool_id_hex`, `sqrt_price_x96`, liquidity, tick, `tick_data`)
    ///   where `tick_data` is a dict of {`tick_index`: (`liquidity_gross`, `liquidity_net`)}
    #[pyo3(signature = (v4_sync_updates, block_number))]
    fn sync_v4_pool_states(
        &self,
        v4_sync_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        let engine = self.engine.lock();
        for item in v4_sync_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 6 {
                let msg = format!(
                    "Expected 6-tuple (pool_manager, pool_id, sqrt_price, liquidity, tick, tick_data), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let pm_str: String = tuple.get_item(0)?.extract()?;
            let pool_manager: Address = pm_str.parse().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager: {e}"))
            })?;
            let pid_str: String = tuple.get_item(1)?.extract()?;
            let pool_id = hex_string_to_pool_id(&pid_str)?;

            let sqrt_price = crate::conversion::alloy::extract_python_u256(&tuple.get_item(2)?)?;
            let liquidity: u128 = tuple.get_item(3)?.extract()?;
            let tick: i32 = tuple.get_item(4)?.extract()?;

            let td_obj = tuple.get_item(5)?;
            let td_dict = td_obj.cast::<pyo3::types::PyDict>()?;
            let mut rust_tick_data = HashMap::new();
            for (key, value) in td_dict.iter() {
                let tick_idx: i32 = key.extract()?;
                let info_tuple = value.cast::<pyo3::types::PyTuple>()?;
                if info_tuple.len() != 2 {
                    let msg = format!(
                        "Expected 2-tuple (liquidity_gross, liquidity_net), got {} elements",
                        info_tuple.len()
                    );
                    return Err(pyo3::exceptions::PyValueError::new_err(msg));
                }
                let liquidity_gross: u128 = info_tuple.get_item(0)?.extract()?;
                let liquidity_net: i128 = info_tuple.get_item(1)?.extract()?;
                rust_tick_data.insert(tick_idx, make_tick_info(liquidity_gross, liquidity_net));
            }

            engine.core.write().sync_v4_pool_state(
                pool_manager,
                pool_id,
                V4StateSync {
                    sqrt_price_x96: sqrt_price,
                    liquidity,
                    tick,
                    tick_data: rust_tick_data,
                    update_block: block_number,
                },
            );
        }
        Ok(())
    }

    /// Process Sync, V3 Swap, and V4 Swap events synchronously (for testing).
    ///
    /// `v2_sync_updates`: list of (`address_str`, `reserve0`, `reserve1`)
    /// `v3_swap_updates`: list of (`address_str`, `sqrt_price_x96`, liquidity, tick, `tick_priors`)
    ///   where `tick_priors` is a list of (`tick_index`, (`liquidity_gross`, `liquidity_net`))
    /// `v4_swap_updates`: list of (`pool_manager_str`, `pool_id_hex`, `sqrt_price_x96`, liquidity, tick, `tick_priors`)
    #[pyo3(signature = (v2_sync_updates, v3_swap_updates, v4_swap_updates, block_number))]
    fn process_logs(
        &self,
        v2_sync_updates: &Bound<'_, PyList>,
        v3_swap_updates: &Bound<'_, PyList>,
        v4_swap_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        let rust_v2 = Self::parse_v2_updates(v2_sync_updates)?;
        let rust_v3 = Self::parse_v3_updates(v3_swap_updates)?;
        let rust_v4 = Self::parse_v4_updates(v4_swap_updates)?;
        self.engine.lock().process_all_updates(
            &rust_v2,
            &rust_v3,
            &rust_v4,
            block_number,
            &BlockMetadata::default(),
        );
        Ok(())
    }
}
