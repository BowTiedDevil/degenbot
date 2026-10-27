//! `PyO3` wrapper for the engine stage surface — solve `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` ,
//! mirroring `crates/engine/degenbot-bot/src/arb_engine/`'s per-concern
//! layout. `PyO3` allows multiple `#[pymethods] impl PyArbEngine { … }`
//! blocks per type, so each concern file contributes one slice.

use super::{hex_string_to_pool_id, Address, PyArbEngine, PyList};
use crate::bot::pool::TickData;
use crate::prelude::*;
use degenbot_pools::families::buffered_event::PoolLiquidityUpdate;
use degenbot_pools::families::state_update::PoolStateUpdate;
use degenbot_pools::state::v3_state::V3StateSync;
use degenbot_pools::state::v4_state::V4StateSync;
use degenbot_substrate::PoolLookup;
use std::sync::Arc;

#[pymethods]
impl PyArbEngine {
    /// Last block number processed by the pump's drain phase. Routes through
    /// the engine's stage surface (`EngineStages`): the engine's own
    /// cursor. The dissolved coordinator cursor (`last_drained_block` under
    /// `drain_lock`) is gone — solve/finalize/publish work runs INLINE in the
    /// single-writer pump driver, so the engine cursor IS the drained cursor
    /// by construction (no Rust/Python race over a mid-drain read).
    ///
    /// Returns `None` if no block has been processed yet (before the first
    /// solve / before resume).
    fn last_processed_block(&self) -> Option<u64> {
        // route through the PumpControl trait (the inherent
        // `EngineStages` twin was hard-cut); the trait cursor is Epoch-typed.
        use degenbot_bot::bot_core::PumpControl;
        PumpControl::last_processed_block(self.driver.stages().as_ref())
            .map(degenbot_substrate::Epoch::block)
    }

    /// Set the last processed block manually after Python backfill.
    #[pyo3(signature = (block))]
    fn set_last_processed_block(&self, py: Python<'_>, block: u64) {
        self.with_stages(py, |e| e.set_last_processed_block(block));
    }

    /// Resolve and solve all registered paths.
    ///
    /// Called to populate results for the first time (replaces the
    /// removed `freeze()` + `initial_solve()`). Subsequent `process_logs`
    /// calls use dependency tracking to only re-solve affected paths.
    fn solve_all_paths(&self, py: Python<'_>, block_number: u64) {
        self.with_stages(py, |e| e.solve_all_paths(block_number));
    }

    /// Set the maximum age (in blocks) for buffered liquidity events.
    ///
    /// Applies to V3 and V4 sub-engine buffers. Pass `None` for unbounded
    /// (no automatic expiry). Events older than `current_block - max_age`
    /// are expired during `process_block`.
    #[pyo3(signature = (max_age))]
    fn set_event_buffer_max_age(&self, py: Python<'_>, max_age: Option<u64>) {
        self.with_stages(py, |e| e.set_event_buffer_max_age(max_age));
    }

    /// Discard all buffered liquidity events for all unregistered pools.
    fn flush_event_buffer(&self, py: Python<'_>) {
        self.with_stages(
            py,
            degenbot_bot::arb_engine::EngineStages::flush_event_buffer,
        );
    }

    /// Number of pools the V2 solve lane admits (the reserve-pair family's
    /// volatile designs — V2 + Aerodrome volatile; ADR-059 D6).
    fn reserve_pair_pool_count(&self, py: Python<'_>) -> usize {
        self.with_stages(
            py,
            degenbot_bot::arb_engine::EngineStages::reserve_pair_pool_count,
        )
    }

    /// Number of registered V3 pools.
    fn v3_pool_count(&self, py: Python<'_>) -> usize {
        self.with_stages(py, degenbot_bot::arb_engine::EngineStages::v3_pool_count)
    }

    /// Number of registered V4 pools.
    fn v4_pool_count(&self, py: Python<'_>) -> usize {
        self.with_stages(py, degenbot_bot::arb_engine::EngineStages::v4_pool_count)
    }

    /// Apply all buffered **backfill** V3 Mint/Burn events for a pool address
    /// on top of the snapshot-seeded tick data.
    ///
    /// Under the shared-BotState design (ADR-006) V3 pools enter `BotState` via
    /// the bot builders (`py_bot.register_v3_pool`, tick data seeded inline),
    /// NOT via the engine's verify-gated `register_v3_pool`. That builder path
    /// seeds the snapshot tick map but never drains the backfill/pump buffer —
    /// so Mint/Burn events that fired during `backfill_from_snapshot` (before
    /// the pool was registered) sit undrained, leaving phantom/missing
    /// liquidity that fails on-chain verification. This drains both the
    /// backfill and the pump buffer in order (backfill first, then pump),
    /// matching what the orphaned `register_v3_pool` path did via
    /// `register_with_concentrated_liquidity_buffers`. Safe to call on an already-registered pool;
    /// a no-op if the pool is unregistered or has no buffered events.
    #[pyo3(signature = (pool_address))]
    fn apply_buffer_v3(&self, py: Python<'_>, pool_address: &str) -> PyResult<()> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let stages = Arc::clone(&self.stages);
        // release the GIL across the `core.write()` hold so the live
        // pump + asyncio loop keep making GIL progress while the main thread
        // awaits the lock. The SINGLE `core.write()` hold across both drains
        // AND the post-drain pin is PRESERVED (the step-2 rolling-start race
        // fix — see `pin_v3_post_drain_snapshot`); `py.detach` wraps the
        // OUTSIDE, it does NOT split the hold. Pre-fix this widened hold was
        // the worst GIL-carrying park in `build_paths` (engine.lock() +
        // core.write() across drain+pin). The closure touches no Python
        // objects.
        py.detach(move || {
            let core = stages.core();
            let mut core = core.write_at(degenbot_substrate::state_lock::LockSite::Python);
            PoolLookup::from_address(*addr).backfill(&mut core);
            PoolLookup::from_address(*addr).pump(&mut core);
            PoolLookup::from_address(addr).pin_post_drain(&mut core);
        });
        Ok(())
    }

    /// Apply all buffered **backfill** V4 `ModifyLiquidity` events for a pool
    /// on top of the snapshot-seeded tick data. V4 analogue of
    /// [`apply_buffer_v3`][Self::apply_buffer_v3].
    #[pyo3(signature = (pool_manager, pool_id_hex))]
    fn apply_buffer_v4(
        &self,
        py: Python<'_>,
        pool_manager: &str,
        pool_id_hex: &str,
    ) -> PyResult<()> {
        let pm = pool_manager.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
        })?;
        let pool_id = crate::bot::engine::hex_string_to_pool_id(pool_id_hex)?;
        let stages = Arc::clone(&self.stages);
        // release the GIL across the `core.write()` hold (V4 twin of
        // `apply_buffer_v3`). The single-write-hold invariant (the step-2
        // race fix) is preserved — `py.detach` wraps the OUTSIDE.
        py.detach(move || {
            let core = stages.core();
            let mut core = core.write_at(degenbot_substrate::state_lock::LockSite::Python);
            PoolLookup::V4 {
                pool_manager: pm,
                pool_id,
            }
            .backfill(&mut core);
            PoolLookup::V4 {
                pool_manager: pm,
                pool_id,
            }
            .pump(&mut core);
            PoolLookup::V4 {
                pool_manager: pm,
                pool_id,
            }
            .pin_post_drain(&mut core);
        });
        Ok(())
    }

    /// Set a V3 pool's registration lifecycle to `Quarantined`. The
    /// live pump then defers the pool's Swap/Mint/Burn events to the pump
    /// buffer until [`set_v3_pool_live`] transitions it back. Call at the
    /// start of `register_v3_pool` (before the first RPC await) so a live
    /// event landing during the drain+pin+verify window cannot advance
    /// `update_block` past `last_complete_block` (the live direct-apply gap
    /// the `drain_pump_completed` buffer gate does NOT cover). No-op for
    /// unregistered pools.
    #[pyo3(signature = (pool_address))]
    fn set_v3_pool_quarantined(&self, py: Python<'_>, pool_address: &str) -> PyResult<()> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        // GIL hygiene: write guard acquired inside the accessor's py.detach.
        self.with_core_mut(py, |s| {
            PoolLookup::from_address(addr).begin_registration_quarantine(&mut *s);
        });
        Ok(())
    }

    /// Set a V4 pool's registration lifecycle to `Quarantined`. V4
    /// twin of [`set_v3_pool_quarantined`]. Call at the start of
    /// `register_v4_pool` (before the first RPC await).
    #[pyo3(signature = (pool_manager, pool_id_hex))]
    fn set_v4_pool_quarantined(
        &self,
        py: Python<'_>,
        pool_manager: &str,
        pool_id_hex: &str,
    ) -> PyResult<()> {
        let pm = pool_manager.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
        })?;
        let pool_id = crate::bot::engine::hex_string_to_pool_id(pool_id_hex)?;
        // GIL hygiene: write guard acquired inside the accessor's py.detach.
        self.with_core_mut(py, |s| {
            PoolLookup::V4 {
                pool_manager: pm,
                pool_id,
            }
            .begin_registration_quarantine(s);
        });
        Ok(())
    }

    /// Transition a V3 pool from `Quarantined` to `Live`: flush the
    /// retained in-progress-block pump tail via the unguarded `drain_pump`
    /// in insertion order, then mark `Live`. Call after step-2 post-drain
    /// verify passes. No-op for unregistered / already-`Live` pools.
    #[pyo3(signature = (pool_address))]
    fn set_v3_pool_live(&self, py: Python<'_>, pool_address: &str) -> PyResult<()> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        // GIL hygiene: write guard acquired inside the accessor's py.detach.
        self.with_core_mut(py, |s| {
            PoolLookup::from_address(addr).mark_registration_live(&mut *s);
        });
        Ok(())
    }

    /// Transition a V4 pool from `Quarantined` to `Live`. V4 twin
    /// of [`set_v3_pool_live`]. Call after step-2 post-drain verify passes.
    #[pyo3(signature = (pool_manager, pool_id_hex))]
    fn set_v4_pool_live(
        &self,
        py: Python<'_>,
        pool_manager: &str,
        pool_id_hex: &str,
    ) -> PyResult<()> {
        let pm = pool_manager.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
        })?;
        let pool_id = crate::bot::engine::hex_string_to_pool_id(pool_id_hex)?;
        // GIL hygiene: write guard acquired inside the accessor's py.detach.
        self.with_core_mut(py, |s| {
            PoolLookup::V4 {
                pool_manager: pm,
                pool_id,
            }
            .mark_registration_live(s);
        });
        Ok(())
    }

    /// Batch-release every pool still `Quarantined` (orphan sweep).
    /// With Tracked pools now registering `Quarantined` by default, call once
    /// after `build_paths` finishes so a Tracked pool built but never reached
    /// by `register_v3/v4_pool` (path skipped before registration) is released
    /// to `Live` instead of deferring events to its buffer indefinitely.
    #[expect(clippy::unnecessary_wraps)]
    fn release_all_v3_v4_quarantined(&self, py: Python<'_>) -> PyResult<()> {
        // GIL hygiene: write guard acquired inside the accessor's py.detach.
        self.with_core_mut(py, degenbot_substrate::BotState::release_all_quarantined);
        Ok(())
    }

    /// Debug/test seam: buffer a V3 backfill liquidity update (Mint/Burn) for
    /// a pool address WITHOUT applying it. If the pool is already registered it
    /// is applied directly (mirroring `BotState::buffer_backfill_v3_liquidity_update`);
    /// otherwise it is buffered for a later [`apply_buffer_v3`] drain. Used to
    /// test `apply_buffer_v3` against a primed buffer without an RPC backfill.
    #[pyo3(signature = (pool_address, tick_lower, tick_upper, liquidity_delta, block_number))]
    fn debug_buffer_v3_liquidity_update(
        &self,
        py: Python<'_>,
        pool_address: &str,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> PyResult<()> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        // GIL hygiene: write guard acquired inside the accessor's py.detach.
        self.with_core_mut(py, |s| {
            s.buffer_backfill_liquidity_update(
                degenbot_substrate::PoolLookup::from_address(addr),
                PoolLiquidityUpdate::new(tick_lower, tick_upper, liquidity_delta, block_number),
            );
        });
        Ok(())
    }

    /// Debug: return the number of buffered liquidity events for a V3 pool address.
    fn debug_v3_buffer_count(&self, py: Python<'_>, pool_address: &str) -> PyResult<usize> {
        let addr = pool_address.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        // GIL hygiene: guards acquired inside the accessor's py.detach.
        let count = self.with_core(py, |s| {
            s.concentrated_liquidity().buffered_v3_event_count(&addr)
        });
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
        // GIL hygiene: guards acquired inside the accessor's py.detach;
        // owned tick data comes out, the dict is built under the GIL below.
        let tick_data = self.with_core(py, |s| {
            let key = s.pool_id_by_address(&addr)?;
            let pool = s.pool_slice::<degenbot_pools::state::v3_state::V3PoolState>(key)?;
            Some(pool.tick_data.clone())
        });
        let Some(tick_data) = tick_data else {
            return Ok(None);
        };

        let dict = pyo3::types::PyDict::new(py);
        for (&tick_idx, info) in &tick_data {
            let lg = info.liquidity_gross.to::<u128>();
            let ln: i128 = info.liquidity_net;
            dict.set_item(tick_idx, (lg, ln))?;
        }
        Ok(Some(dict))
    }

    /// Number of registered paths.
    fn path_count(&self, py: Python<'_>) -> usize {
        // GIL hygiene: engine Mutex acquired inside the accessor's py.detach.
        self.with_stages(py, degenbot_bot::arb_engine::EngineStages::path_count)
    }

    /// Snapshot the engine-owned state for every hop in a registered path.
    ///
    /// This is a diagnostic helper for investigating simulation failures.
    /// Snapshots the engine-owned pool state for every hop in `path_id` (no
    /// RPC calls — pure engine-state read under the engine lock).
    ///
    /// `rpc_url` is accepted for forward compatibility but currently ignored;
    /// the on-chain recompute half (`fetch_onchain`) was retired when
    /// `[sim-diag]` moved onto the inspector's captured swaps: captured
    /// swaps ARE byte-exact ground truth (proven via
    /// the `swap_capture_correctness` mainnet probe), so no onchain re-fetch
    /// is needed to classify a revert.
    ///
    /// Returns a Python `dict` containing `path_id`, `path_type`, `solve_block`,
    /// `engine_processed_block`, and a `hops` list with per-hop engine state.
    ///
    /// Raises `KeyError` if `path_id` is not registered.
    #[expect(clippy::needless_pass_by_value)]
    #[pyo3(signature = (path_id, rpc_url=None))]
    fn diagnostic_inspect_path(
        &self,
        py: Python<'_>,
        path_id: u64,
        rpc_url: Option<String>,
    ) -> PyResult<pyo3::Py<pyo3::PyAny>> {
        let _ = rpc_url; // retained for API stability; onchain fetch retired.
                         // GIL hygiene: engine Mutex acquired inside the accessor's py.detach.
        let snapshot = self.with_stages(py, |e| e.diagnostic_path_state(path_id));

        let Some(snapshot) = snapshot else {
            return Err(pyo3::exceptions::PyKeyError::new_err(format!(
                "path_id {path_id} is not registered"
            )));
        };

        // Convert the snapshot to a Python dict via JSON round-trip.
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
        py: Python<'_>,
        v3_sync_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        // GIL hygiene (incident 2026-08-20 class): extract ALL Python data
        // first, under the GIL with no locks held; then apply the writes in
        // one py.detach scope. A write guard must never be held across Python
        // API calls, and the GIL must never be held while parked on the lock.
        let mut updates = Vec::with_capacity(v3_sync_updates.len());
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
            let rust_tick_data = TickData::from_py_pairs(td_dict.as_any(), block_number)?;

            updates.push((addr, sqrt_price, liquidity, tick, rust_tick_data));
        }

        let refusal = self.with_core_mut(py, |s| {
            updates
                .into_iter()
                .find_map(|(addr, sqrt_price, liquidity, tick, tick_data)| {
                    s.sync_pool_state(
                        PoolLookup::from_address(addr),
                        PoolStateUpdate::V3(V3StateSync {
                            sqrt_price_x96: sqrt_price,
                            liquidity,
                            tick,
                            tick_data,
                            update_block: block_number,
                        }),
                    )
                    .err()
                })
        });
        match refusal {
            None => Ok(()),
            Some(refusal) => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "sync_v3_pool_states refused: {refusal}"
            ))),
        }
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
        py: Python<'_>,
        v4_sync_updates: &Bound<'_, PyList>,
        block_number: u64,
    ) -> PyResult<()> {
        // GIL hygiene: extract ALL Python data first (under the GIL, no locks
        // held), then apply the writes inside one py.detach scope.
        let mut updates = Vec::with_capacity(v4_sync_updates.len());
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
            let rust_tick_data = TickData::from_py_pairs(td_dict.as_any(), block_number)?;

            updates.push((
                pool_manager,
                pool_id,
                V4StateSync {
                    sqrt_price_x96: sqrt_price,
                    liquidity,
                    tick,
                    tick_data: rust_tick_data,
                    update_block: block_number,
                },
            ));
        }

        let refusal = self.with_core_mut(py, |s| {
            updates
                .into_iter()
                .find_map(|(pool_manager, pool_id, sync)| {
                    s.sync_pool_state(
                        PoolLookup::V4 {
                            pool_manager,
                            pool_id,
                        },
                        PoolStateUpdate::V4(sync),
                    )
                    .err()
                })
        });
        match refusal {
            None => Ok(()),
            Some(refusal) => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "sync_v4_pool_states refused: {refusal}"
            ))),
        }
    }
}
