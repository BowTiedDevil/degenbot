//! `PyO3` wrapper for the engine stage surface — `result_channel` `#[pymethods]` slice.
//!
//! One concern per file, mirroring
//! `crates/engine/degenbot-bot/src/arb_engine/`'s layout: `PyO3` allows
//! multiple `#[pymethods] impl PyArbEngine { … }` blocks per type, so each
//! concern file contributes one slice.

use super::{Address, EngineStages, HopType, PyArbEngine, PyDict, PyList, U256};
use crate::prelude::*;
use degenbot_pools::solve::MixedPoolRef;

#[pymethods]
impl PyArbEngine {
    /// Read the last solved results and block number.
    ///
    /// Inspect a registered path by ID.
    ///
    /// Returns a dict with:
    ///   - "`path_id"`: int
    ///   - "hops": list of dicts, each with:
    ///     - "type": the hop's internal family tag (`"v2"` / `"v3"` / `"v4"` /
    ///       `"aerodrome-v2"` / `"balancer-weighted"` / `"balancer-stable"` /
    ///       `"curve"` — `HopType::as_tag`)
    ///     - "address": str (V2/V3 contract address, or V4 `pool_manager`)
    ///     - "`pool_id"`: str (V4 only — the pool ID hex)
    ///     - "`zero_for_one"`: bool
    ///     - "fee": int (V2: `gamma_numer`; V3: pool fee; V4: pool fee)
    ///     - "`tick_spacing"`: int (V3/V4 only)
    ///   Returns None if the `path_id` is not found.
    #[pyo3(signature = (path_id))]
    fn inspect_path(&self, path_id: u64, py: Python<'_>) -> PyResult<Option<Py<PyDict>>> {
        let Some(pool_refs) = self.collect_pool_refs(py, path_id) else {
            return Ok(None);
        };
        let hops = self.read_hop_infos(py, &pool_refs);
        Self::render_hops(py, path_id, &hops).map(Some)
    }

    /// Returns (`results`, `block_number`) where results is a flat list:
    /// [`path_id_0`, `optimal_input_0`, `profit_0`, `path_id_1`, ...]
    fn latest_results(&self, py: Python<'_>) -> PyResult<(Py<PyList>, u64)> {
        // GIL hygiene: engine Mutex acquired inside the accessor's py.detach.
        let (results, block_num) = self.with_stages(py, EngineStages::latest_results);

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
        self.with_stages(py, |e| e.deregister_path(path_id))
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
        self.with_stages(py, |e| e.set_profit_thresholds(min, max));
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
        Ok(self.with_core(py, |core| {
            let pool_id = core.pool_id_by_address(&addr)?;
            let state = core.pool_slice::<degenbot_pools::V2PoolState>(pool_id)?;
            Some((
                pool_id,
                state.reserve0.to_string(),
                state.reserve1.to_string(),
            ))
        }))
    }
}

/// The three GIL phases of [`PyArbEngine::inspect_path`], split so each one's
/// lock discipline is carried by its signature instead of a comment: owned refs
/// out of the engine, owned hop data out of the core, Python objects under the
/// GIL.
impl PyArbEngine {
    /// Resolve the path's immutable per-hop pool refs.
    ///
    /// GIL hygiene: the engine Mutex is acquired inside the accessor's
    /// `py.detach`, so the guard never coexists with the GIL. `None` means the
    /// path is not registered.
    fn collect_pool_refs(&self, py: Python<'_>, path_id: u64) -> Option<Vec<MixedPoolRef>> {
        self.with_stages(py, |e| e.path_pool_refs(path_id))
    }

    /// Read each pool ref's identity and build its owned [`HopInfo`].
    ///
    /// GIL hygiene: the engine Mutex and the core read guard are both acquired
    /// inside the accessor's `py.detach`, and the loop is pure Rust — owned hop
    /// data comes out, so no Python API call runs while a state guard is held.
    ///
    /// ADR-003: V2 state lives in `BotState`, so ONE core-lock window covers
    /// every lookup in this loop (engine-then-core ordering).
    fn read_hop_infos(&self, py: Python<'_>, pool_refs: &[MixedPoolRef]) -> Vec<HopInfo> {
        self.with_core(py, |core| {
            pool_refs
                .iter()
                .map(|pool_ref| match pool_ref.hop_type {
                    HopType::V2 => {
                        let identity =
                            core.pool_slice::<degenbot_pools::V2PoolIdentity>(pool_ref.pool_key);
                        // V2 fee is `gamma_numer`, orientation-selected (ADR-003).
                        let gamma_numer = identity.map(|i| {
                            if pool_ref.zero_for_one {
                                i.fee_token0.gamma_numer()
                            } else {
                                i.fee_token1.gamma_numer()
                            }
                        });
                        HopInfo {
                            address: identity.map(|i| format!("{}", i.address)),
                            fee: gamma_numer,
                            ..HopInfo::new(pool_ref)
                        }
                    }
                    HopType::V3 => {
                        let identity =
                            core.pool_slice::<degenbot_pools::V3PoolIdentity>(pool_ref.pool_key);
                        let (addr, fee, ts) = identity.map_or((None, None, None), |i| {
                            (
                                Some(format!("{}", i.address)),
                                Some(u64::from(i.fee)),
                                Some(i.tick_spacing),
                            )
                        });
                        HopInfo {
                            address: addr,
                            fee,
                            tick_spacing: ts,
                            ..HopInfo::new(pool_ref)
                        }
                    }
                    HopType::V4 => {
                        let identity =
                            core.pool_slice::<degenbot_pools::V4PoolIdentity>(pool_ref.pool_key);
                        let (pm, pid, fee, ts) = identity.map_or((None, None, None, None), |i| {
                            (
                                Some(format!("{}", i.pool_manager)),
                                Some(format!("0x{}", alloy::hex::encode(i.pool_id))),
                                Some(u64::from(i.pool_key.fee)),
                                Some(i.pool_key.tick_spacing),
                            )
                        });
                        HopInfo {
                            address: pm,
                            pool_id: pid,
                            fee,
                            tick_spacing: ts,
                            ..HopInfo::new(pool_ref)
                        }
                    }
                    // Solidly hop-info lands with the build/register plumbing
                    //. Until then, a Solidly hop emits a minimal
                    // HopInfo so the result-channel match is exhaustive; the
                    // resolve short-circuit means no Solidly hop reaches a solve.
                    HopType::SolidlyStable => HopInfo::new(pool_ref),
                    HopType::BalancerWeighted => {
                        let id = core.pool_slice::<degenbot_pools::BalancerWeightedPoolIdentity>(
                            pool_ref.pool_key,
                        );
                        HopInfo {
                            address: id.map(|i| format!("{}", i.address)),
                            ..HopInfo::new(pool_ref)
                        }
                    }
                    HopType::BalancerStable => {
                        let id = core.pool_slice::<degenbot_pools::BalancerStablePoolIdentity>(
                            pool_ref.pool_key,
                        );
                        HopInfo {
                            address: id.map(|i| format!("{}", i.address)),
                            ..HopInfo::new(pool_ref)
                        }
                    }
                    HopType::CurveStableswap => {
                        let id =
                            core.pool_slice::<degenbot_pools::CurvePoolIdentity>(pool_ref.pool_key);
                        HopInfo {
                            address: id.map(|i| format!("{}", i.address)),
                            ..HopInfo::new(pool_ref)
                        }
                    }
                })
                .collect()
        })
    }

    /// Project the owned hop data into the Python result dict.
    ///
    /// Runs under the GIL by construction: it takes no engine access and no
    /// state guard, only owned [`HopInfo`] values.
    fn render_hops(py: Python<'_>, path_id: u64, hops: &[HopInfo]) -> PyResult<Py<PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("path_id", path_id)?;

        let hops_list = PyList::empty(py);
        for hop in hops {
            hops_list.append(hop_info_to_pydict(py, hop)?)?;
        }
        dict.set_item("hops", hops_list)?;

        Ok(dict.unbind())
    }
}

/// One hop's view for [`PyArbEngine::inspect_path`], built from the
/// engine's sub-states before being projected into a Python dict by
/// [`hop_info_to_pydict`].
struct HopInfo {
    hop_type: &'static str,
    address: Option<String>,
    pool_id: Option<String>,
    zero_for_one: bool,
    fee: Option<u64>,
    tick_spacing: Option<i32>,
}

impl HopInfo {
    /// Every hop's shared fields; a family's arm then sets only what its
    /// identity supplies.
    fn new(pool_ref: &MixedPoolRef) -> Self {
        Self {
            hop_type: pool_ref.hop_type.as_tag(),
            address: None,
            pool_id: None,
            zero_for_one: pool_ref.zero_for_one,
            fee: None,
            tick_spacing: None,
        }
    }
}

/// Build the Python dict describing a single hop for `inspect_path`.
fn hop_info_to_pydict<'py>(py: Python<'py>, hop: &HopInfo) -> PyResult<Bound<'py, PyDict>> {
    let hop_dict = PyDict::new(py);
    hop_dict.set_item("type", hop.hop_type)?;
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
