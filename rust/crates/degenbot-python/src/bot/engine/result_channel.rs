//! `PyO3` wrapper for the `ArbitrageEngine` — `result_channel` `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` (ergo UG6FKN task 74W2Z6),
//! mirroring `crates/degenbot-bot/src/solvers/arb_engine/`'s per-concern
//! layout. `PyO3` allows multiple `#[pymethods] impl PyArbitrageEngine { … }`
//! blocks per type, so each concern file contributes one slice.

use super::{
    mpsc, Address, Arc, BlockNotification, HopType, PyArbitrageEngine, PyDict, PyList,
    PyStopAsyncIteration, ResultBatch, SolvePathResult, U256,
};
use crate::prelude::*;

#[pymethods]
impl PyArbitrageEngine {
    /// Read the last solved results and block number.
    ///
    /// Inspect a registered path by ID.
    ///
    /// Returns a dict with:
    ///   - "`path_id"`: int
    ///   - "hops": list of dicts, each with:
    ///     - "type": "V2" | "V3" | "V4"
    ///     - "address": str (V2/V3 contract address, or V4 `pool_manager`)
    ///     - "`pool_id"`: str (V4 only — the pool ID hex)
    ///     - "`zero_for_one"`: bool
    ///     - "fee": int (V2: `gamma_numer`; V3: pool fee; V4: pool fee)
    ///     - "`tick_spacing"`: int (V3/V4 only)
    ///   Returns None if the `path_id` is not found.
    #[pyo3(signature = (path_id))]
    #[expect(clippy::too_many_lines)]
    fn inspect_path(&self, path_id: u64, py: Python<'_>) -> PyResult<Option<Py<PyDict>>> {
        // Phase 1: Collect pool refs from the path. GIL hygiene: the engine
        // Mutex is acquired inside the accessor's py.detach.
        let Some(pool_refs) = self.with_engine(py, |e| {
            e.path_pools().get(&path_id).map(|p| p.pools.clone())
        }) else {
            return Ok(None);
        };

        // Phase 2: Query sub-engines for pool details. GIL hygiene: the
        // engine Mutex + core read guard are acquired inside the accessor's
        // py.detach; the loop is pure Rust (owned HopInfo data out, no Python
        // API under the locks) and the dict is built under the GIL below.
        // ADR-003: V2 state lives in BotState. One core-lock window covers all
        // V2 lookups in this loop (engine-then-core ordering; V3/V4 state still
        // reads the per-family engines, which are disjoint fields).
        let hops: Vec<HopInfo> = self.with_engine_core(py, |core| {
            let mut hops = Vec::new();

            for pool_ref in &pool_refs {
                match pool_ref.hop_type {
                    HopType::V2 => {
                        let identity = core.get_v2_identity(pool_ref.pool_key);
                        let addr = identity.map(|i| format!("{}", i.address));
                        // V2 fee is `gamma_numer`, orientation-selected (ADR-003).
                        let gamma_numer = identity.map(|i| {
                            if pool_ref.zero_for_one {
                                i.fee_token0.0
                            } else {
                                i.fee_token1.0
                            }
                        });
                        hops.push(HopInfo {
                            hop_type: "V2".to_string(),
                            address: addr,
                            pool_id: None,
                            zero_for_one: pool_ref.zero_for_one,
                            fee: gamma_numer,
                            tick_spacing: None,
                        });
                    }
                    HopType::V3 => {
                        let pool_id = pool_ref.pool_key;
                        let identity = core.get_v3_identity(pool_id);
                        let (addr, fee, ts) = identity.map_or((None, None, None), |i| {
                            (
                                Some(format!("{}", i.address)),
                                Some(u64::from(i.fee)),
                                Some(i.tick_spacing),
                            )
                        });
                        hops.push(HopInfo {
                            hop_type: "V3".to_string(),
                            address: addr,
                            pool_id: None,
                            zero_for_one: pool_ref.zero_for_one,
                            fee,
                            tick_spacing: ts,
                        });
                    }
                    HopType::V4 => {
                        let identity = core.get_v4_identity(pool_ref.pool_key);
                        let (pm, pid, fee, ts) = identity.map_or((None, None, None, None), |i| {
                            (
                                Some(format!("{}", i.pool_manager)),
                                Some(format!("0x{}", alloy::hex::encode(i.pool_id))),
                                Some(u64::from(i.pool_key.fee)),
                                Some(i.pool_key.tick_spacing),
                            )
                        });
                        hops.push(HopInfo {
                            hop_type: "V4".to_string(),
                            address: pm,
                            pool_id: pid,
                            zero_for_one: pool_ref.zero_for_one,
                            fee,
                            tick_spacing: ts,
                        });
                    }
                    // Solidly hop-info lands with the build/register plumbing
                    // (task WCT5KR). Until then, a Solidly hop emits a minimal
                    // HopInfo so the result-channel match is exhaustive; the
                    // resolve short-circuit means no Solidly hop reaches a solve.
                    HopType::SolidlyStable => {
                        hops.push(HopInfo {
                            hop_type: "Solidly".to_string(),
                            address: None,
                            pool_id: None,
                            zero_for_one: pool_ref.zero_for_one,
                            fee: None,
                            tick_spacing: None,
                        });
                    }
                    HopType::BalancerWeighted => {
                        let id = core.get_balancer_weighted_identity(pool_ref.pool_key);
                        hops.push(HopInfo {
                            hop_type: "BalW".to_string(),
                            address: id.map(|i| format!("{}", i.address)),
                            pool_id: None,
                            zero_for_one: pool_ref.zero_for_one,
                            fee: None,
                            tick_spacing: None,
                        });
                    }
                    HopType::BalancerStable => {
                        let id = core.get_balancer_stable_identity(pool_ref.pool_key);
                        hops.push(HopInfo {
                            hop_type: "BalS".to_string(),
                            address: id.map(|i| format!("{}", i.address)),
                            pool_id: None,
                            zero_for_one: pool_ref.zero_for_one,
                            fee: None,
                            tick_spacing: None,
                        });
                    }
                    HopType::CurveStableswap => {
                        let id = core.get_curve_identity(pool_ref.pool_key);
                        hops.push(HopInfo {
                            hop_type: "Crv".to_string(),
                            address: id.map(|i| format!("{}", i.address)),
                            pool_id: None,
                            zero_for_one: pool_ref.zero_for_one,
                            fee: None,
                            tick_spacing: None,
                        });
                    }
                }
            }

            hops
        });

        // Phase 3: Build the Python dict
        let dict = PyDict::new(py);
        dict.set_item("path_id", path_id)?;

        let hops_list = PyList::empty(py);
        for hop in &hops {
            hops_list.append(hop_info_to_pydict(py, hop)?)?;
        }
        dict.set_item("hops", hops_list)?;

        Ok(Some(dict.unbind()))
    }

    /// Returns (`results`, `block_number`) where results is a flat list:
    /// [`path_id_0`, `optimal_input_0`, `profit_0`, `path_id_1`, ...]
    fn latest_results(&self, py: Python<'_>) -> PyResult<(Py<PyList>, u64)> {
        // GIL hygiene: engine Mutex acquired inside the accessor's py.detach.
        let (results, block_num) = self.with_engine(py, |e| {
            let (results, block) = e.latest_results();
            (results.clone(), block)
        });

        let py_list = PyList::empty(py);
        for (path_id, solve_result) in results {
            let path_id_py = path_id.into_pyobject(py)?;
            let input_py =
                crate::conversion::alloy::PyU256(solve_result.optimal_input).into_pyobject(py)?;
            let profit_py =
                crate::conversion::alloy::PyU256(solve_result.profit).into_pyobject(py)?;

            // Build hop_outputs as a Python tuple
            let hop_outputs_py = PyList::empty(py);
            for hop_out in &solve_result.hop_outputs {
                let hop_py = crate::conversion::alloy::PyU256(*hop_out).into_pyobject(py)?;
                hop_outputs_py.append(hop_py)?;
            }
            let hop_tuple = hop_outputs_py.into_pyobject(py)?;

            // Build consumed_inputs as a Python tuple
            let consumed_inputs_py = PyList::empty(py);
            for consumed in &solve_result.consumed_inputs {
                let consumed_py = crate::conversion::alloy::PyU256(*consumed).into_pyobject(py)?;
                consumed_inputs_py.append(consumed_py)?;
            }
            let consumed_tuple = consumed_inputs_py.into_pyobject(py)?;

            let result_tuple =
                (path_id_py, input_py, profit_py, hop_tuple, consumed_tuple).into_pyobject(py)?;
            py_list.append(result_tuple)?;
        }

        Ok((py_list.unbind(), block_num))
    }

    /// De-register a path from the engine.
    ///
    /// Removes the path from the engine's internal state. The path's pools
    /// are **not** removed — other paths may still reference them.
    ///
    /// Returns `true` if the path existed and was removed.
    #[pyo3(signature = (path_id))]
    fn deregister_path(&self, py: Python<'_>, path_id: u64) -> bool {
        self.with_engine_mut(py, |e| e.deregister_path(path_id))
    }

    /// Set the profit thresholds for the result batch channel.
    ///
    /// Only paths with `profit > min_profit` and `profit <= max_profit`
    /// appear in batch `fresh` / `updated` entries.
    ///
    /// Both bounds accept full `uint256`-scale Python integers (matching the
    /// `U256` profits the solver produces); the previous `u64` signature
    /// silently dropped any profit above `u64::MAX` (~1.84e19), which is
    /// reachable for 18-decimal tokens with large reserves and within the
    /// V4 `int128` guard's `2^127-1` ceiling.
    ///
    /// `max_profit` defaults to `None`, which means "no upper cap"
    /// (`U256::MAX`) — the only safe unbounded setting. Callers should prefer
    /// `max_profit=None` over passing `0`, since `max_profit == 0` excludes
    /// every non-negative profit under the inclusive (`<=`) bound.
    #[pyo3(signature = (min_profit, max_profit=None))]
    fn set_profit_thresholds(
        &self,
        py: Python<'_>,
        min_profit: &Bound<'_, pyo3::PyAny>,
        max_profit: Option<&Bound<'_, pyo3::PyAny>>,
    ) -> PyResult<()> {
        let min = crate::conversion::alloy::extract_python_u256(min_profit)?;
        let max = match max_profit {
            Some(obj) => crate::conversion::alloy::extract_python_u256(obj)?,
            None => U256::MAX,
        };
        self.with_engine_mut(py, |e| e.set_profit_thresholds(min, max));
        Ok(())
    }

    /// DIAG-a3f2: Dump V2 pool state for a given address.
    /// Returns (`pool_id`, `reserve0`, `reserve1`) or None.
    ///
    /// ADR-003: V2 state lives in `BotState` as a single entry per address
    /// (orientation is selected at solve time via `zero_for_one`, not by a
    /// separate reverse key). The former forward/reverse dual keys are gone.
    #[pyo3(signature = (address_hex))]
    fn diag_v2_pool(
        &self,
        py: Python<'_>,
        address_hex: &str,
    ) -> PyResult<Option<(u64, String, String)>> {
        let addr: Address = address_hex.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
        })?;
        // GIL hygiene: guards acquired inside the accessor's py.detach;
        // owned data comes out, the PyResult is assembled under the GIL.
        Ok(self.with_engine_core(py, |core| {
            let pool_id = core.pool_id_by_address(&addr)?;
            let state = core.get_v2_pool_state(pool_id)?;
            Some((
                pool_id,
                state.reserve0.to_string(),
                state.reserve1.to_string(),
            ))
        }))
    }

    /// Return self as an async iterator over result batches.
    #[expect(clippy::missing_const_for_fn)]
    fn __aiter__(slf: PyClassGuard<'_, Self>) -> PyClassGuard<'_, Self> {
        slf
    }

    /// Return a fresh async iterator over `newHeads` block notifications
    /// (epic 6W35AI).
    ///
    /// The settlement-arbitrage bot must derive its block clock from this stream, NOT
    /// from the result batch's `solve_block` field — `solve_block` lags by
    /// the send debounce and only advances when a batch is actually sent,
    /// so using it as the clock freezes `[block: N]` behind the pump's
    /// `current_block`. The pump forwards exactly one `BlockNotification`
    /// per accepted `WsEvent::BlockHeader` (in lockstep with the block the
    /// solver just solved against), so this stream is the authoritative clock.
    ///
    /// Each yielded item is a dict: `number`, `timestamp`,
    /// `base_fee_per_gas` (int | None), `gas_used`, `gas_limit`.
    ///
    /// Can be called at most once (the receiver is moved into the iterator).
    /// Subsequent calls raise `RuntimeError`.
    fn block_stream(&self) -> PyResult<BlockStream> {
        let block_rx = self.block_rx.lock().take().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err("block_stream() can only be called once")
        })?;
        Ok(BlockStream::new(block_rx))
    }

    /// Await the next result batch from the engine.
    ///
    /// Returns a dict with keys:
    ///   - "`solve_block"`: int
    ///   - "timestamp": int
    ///   - "`base_fee_per_gas"`: int | None
    ///   - "`gas_used"`: int
    ///   - "`gas_limit"`: int
    ///   - "fresh": list of (`path_id`, `optimal_input`, profit, `hop_outputs`, `consumed_inputs`)
    ///   - "updated": list of (`path_id`, `optimal_input`, profit, `hop_outputs`, `consumed_inputs`)
    ///   - "expired": list of int (`path_ids`)
    ///   - "removed": list of int (`path_ids`)
    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let result_rx = Arc::clone(&self.result_rx);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            // Take the receiver for awaiting
            let mut rx = result_rx
                .lock()
                .take()
                .ok_or_else(|| PyStopAsyncIteration::new_err("Result channel closed"))?;

            // Wait for the next batch
            let batch = rx.recv().await.ok_or_else(|| {
                PyStopAsyncIteration::new_err("Result channel closed — pump may have stopped.")
            })?;

            // Put the receiver back
            *result_rx.lock() = Some(rx);

            // Convert batch to Python dict (requires GIL)
            Python::attach(|py| batch_to_py_dict(&batch, py))
        })
    }
}

// --- result-channel free helpers ---
/// Helper to construct `TickInfo` from Python-extracted values.
/// Convert a `ResultBatch` to a Python dict.
///
/// Called under the GIL after receiving a batch from the result channel.
fn batch_to_py_dict(batch: &ResultBatch, py: Python<'_>) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("solve_block", batch.solve_block)?;
    dict.set_item("timestamp", batch.timestamp)?;
    dict.set_item("base_fee_per_gas", batch.base_fee_per_gas)?;
    dict.set_item("gas_used", batch.gas_used)?;
    dict.set_item("gas_limit", batch.gas_limit)?;

    // fresh: list of (path_id, optimal_input, profit, hop_outputs, consumed_inputs)
    let fresh_list = PyList::empty(py);
    for (path_id, result) in &batch.fresh {
        let tuple = solve_result_to_py_tuple(*path_id, result, py)?;
        fresh_list.append(tuple)?;
    }
    dict.set_item("fresh", fresh_list)?;

    // updated: same format as fresh
    let updated_list = PyList::empty(py);
    for (path_id, result) in &batch.updated {
        let tuple = solve_result_to_py_tuple(*path_id, result, py)?;
        updated_list.append(tuple)?;
    }
    dict.set_item("updated", updated_list)?;

    // expired: list of path_ids
    let expired_list = PyList::empty(py);
    for &path_id in &batch.expired {
        expired_list.append(path_id)?;
    }
    dict.set_item("expired", expired_list)?;

    // removed: list of path_ids
    let removed_list = PyList::empty(py);
    for &path_id in &batch.removed {
        removed_list.append(path_id)?;
    }
    dict.set_item("removed", removed_list)?;

    Ok(dict.unbind())
}
/// Convert a (`path_id`, `SolvePathResult`) to a Python tuple.
fn solve_result_to_py_tuple<'py>(
    path_id: u64,
    result: &SolvePathResult,
    py: Python<'py>,
) -> PyResult<Bound<'py, pyo3::types::PyTuple>> {
    let path_id_py = path_id.into_pyobject(py)?;
    let input_py = crate::conversion::alloy::PyU256(result.optimal_input).into_pyobject(py)?;
    let profit_py = crate::conversion::alloy::PyU256(result.profit).into_pyobject(py)?;

    let hop_outputs_py = PyList::empty(py);
    for hop_out in &result.hop_outputs {
        let hop_py = crate::conversion::alloy::PyU256(*hop_out).into_pyobject(py)?;
        hop_outputs_py.append(hop_py)?;
    }

    let consumed_inputs_py = PyList::empty(py);
    for consumed in &result.consumed_inputs {
        let consumed_py = crate::conversion::alloy::PyU256(*consumed).into_pyobject(py)?;
        consumed_inputs_py.append(consumed_py)?;
    }

    let state_nonces_py = PyList::empty(py);
    for &nonce in &result.state_nonces {
        state_nonces_py.append(nonce.into_pyobject(py)?)?;
    }

    (
        path_id_py,
        input_py,
        profit_py,
        hop_outputs_py,
        consumed_inputs_py,
        state_nonces_py,
    )
        .into_pyobject(py)
}
/// One hop's view for [`PyArbitrageEngine::inspect_path`], built from the
/// engine's sub-states before being projected into a Python dict by
/// [`hop_info_to_pydict`].
struct HopInfo {
    hop_type: String,
    address: Option<String>,
    pool_id: Option<String>,
    zero_for_one: bool,
    fee: Option<u64>,
    tick_spacing: Option<i32>,
}
/// Build the Python dict describing a single hop for `inspect_path`.
fn hop_info_to_pydict<'py>(py: Python<'py>, hop: &HopInfo) -> PyResult<Bound<'py, PyDict>> {
    let hop_dict = PyDict::new(py);
    hop_dict.set_item("type", hop.hop_type.as_str())?;
    if let Some(ref a) = hop.address {
        hop_dict.set_item("address", a)?;
    }
    if let Some(ref pid) = hop.pool_id {
        hop_dict.set_item("pool_id", pid)?;
    }
    hop_dict.set_item("zero_for_one", hop.zero_for_one)?;
    if let Some(f) = hop.fee {
        hop_dict.set_item("fee", f)?;
    }
    if let Some(ts) = hop.tick_spacing {
        hop_dict.set_item("tick_spacing", ts)?;
    }
    Ok(hop_dict)
}

// ── Block stream (epic 6W35AI) ─────────────────────────────────────────────
//
// The authoritative `newHeads`-derived block clock for Python. The pump
// forwards one `BlockNotification` per accepted header via
// `DrainSink::notify_block`; this class is the async iterator Python consumes
// (`async for block in engine.block_stream(): …`). Mirrors the result-channel
// `__anext__` shape (take the receiver, await + send, put it back) so a single
// shared receiver survives across awaits.

/// pyo3 async iterator over `newHeads` block notifications.
#[pyclass(name = "BlockStream", skip_from_py_object, module = "degenbot._ffi")]
pub struct BlockStream {
    /// The block-notification receiver. `Option` + `put-back` mirrors
    /// `PyArbitrageEngine::result_rx` so the coroutine can re-share the
    /// receiver across `__anext__` calls.
    block_rx: Arc<parking_lot::Mutex<Option<mpsc::UnboundedReceiver<BlockNotification>>>>,
}

impl BlockStream {
    /// Construct from the receiver handed out by `PyArbitrageEngine::block_stream`.
    #[must_use]
    pub fn new(block_rx: mpsc::UnboundedReceiver<BlockNotification>) -> Self {
        Self {
            block_rx: Arc::new(parking_lot::Mutex::new(Some(block_rx))),
        }
    }
}

#[pymethods]
impl BlockStream {
    /// Return self as the async iterator.
    #[expect(clippy::missing_const_for_fn)]
    fn __aiter__(slf: PyClassGuard<'_, Self>) -> PyClassGuard<'_, Self> {
        slf
    }

    /// Await the next block notification from the pump.
    ///
    /// Returns a dict with keys: `number` (int), `timestamp` (int),
    /// `base_fee_per_gas` (int | None), `gas_used` (int), `gas_limit` (int).
    /// Raises `StopAsyncIteration` when the channel closes (pump stopped).
    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let block_rx = Arc::clone(&self.block_rx);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut rx = block_rx
                .lock()
                .take()
                .ok_or_else(|| PyStopAsyncIteration::new_err("Block channel closed"))?;

            let notif = rx.recv().await.ok_or_else(|| {
                PyStopAsyncIteration::new_err("Block channel closed — pump may have stopped.")
            })?;

            *block_rx.lock() = Some(rx);

            Python::attach(|py| notif_to_py_dict(&notif, py))
        })
    }
}

/// Convert a `BlockNotification` to a Python dict (called under the GIL).
fn notif_to_py_dict(notif: &BlockNotification, py: Python<'_>) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("number", notif.number)?;
    dict.set_item("timestamp", notif.timestamp)?;
    dict.set_item("base_fee_per_gas", notif.base_fee_per_gas)?;
    dict.set_item("gas_used", notif.gas_used)?;
    dict.set_item("gas_limit", notif.gas_limit)?;
    Ok(dict.unbind())
}
