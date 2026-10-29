//! The inline-sim payload seam — one categorization for both entry arms.
//!
//! Two entry arms reach one sim seam: the FFI batch executor (`simulation::batch`)
//! and the engine's inline-sim payload records. [`merge_payload_results_py`]
//! resolves each payload's
//! typed hops through the engine and drives the SAME crate-owned join
//! (`degenbot_batch_executor::assembly::merge_payload_results`) the batch arm
//! uses, so the mutual-exclusion pool keys and the profit-floor verdict have one
//! home. Python renders the returned rows/verdicts only.
//!
//! Boundary discipline (ADR-005): argument extraction under the GIL, the join
//! crate-side, and the Python view built at the boundary — no business logic in
//! this wrapper.

use crate::prelude::*;
use crate::simulation::render::path_info_to_py_dict;
use degenbot_batch_executor::assembly::{merge_payload_results, PayloadArm};
use degenbot_batch_executor::row::{PayloadFailure, PayloadRow};
use degenbot_batch_executor::PathResolver;
use degenbot_executor::composers::PathInfo;
use degenbot_submission::SubmitCandidate;
use pyo3::exceptions::PyValueError;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────────────────
// The inline-sim payload seam — one sim seam for both entry arms
// ─────────────────────────────────────────────────────────────────────────
//
// The engine's inline-sim payloads reach the same categorization as the FFI
// batch: this seam reuses the batch's own join (join_sim_result →
// derive_path_pools) and the core's is_gas_profitable predicate, so the
// mutual-exclusion pool keys and the profit-floor verdict are evaluated by
// one rule, here, for both arms. Python renders the returned rows/verdicts
// only.

/// A resolved-map bridge over the crate's `PathResolver`: the GIL-held
/// resolve phase (below) feeds it, and the crate merge consumes it — plain
/// map data, so it is `Send + Sync` where a GIL token cannot be.
struct ResolvedPaths<'a> {
    map: &'a HashMap<u64, PathInfo>,
}

impl PathResolver for ResolvedPaths<'_> {
    fn resolve(&self, path_id: u64) -> Option<PathInfo> {
        self.map.get(&path_id).cloned()
    }
}

/// The payload arm of the sim seam: derive the dispatch-policy
/// facts for the engine's inline-sim payload records Rust-side, through the
/// SAME row builder the FFI batch join uses.
///
/// Each payload dict is the `result_channel` serializer's primitive field-set
/// (`path_id` / `gross_profit` / `net_profit` / `gas_used` / `priority_fee` /
/// `base_fee_next` / `execute_calldata` / `access_list` / `failure`). Per entry:
///
/// 1. The registered path's typed hops are resolved via the SAME
///    engine projection the FFI batch candidates use
///    (`PyArbEngine::path_info_for_core`) and routed through
///    `join_sim_result` → `derive_path_pools` — the mutual-exclusion set is
///    byte-identical to the FFI batch row for the same path id, by
///    construction (V4 → `pool_id_hex`; V2/V3 → EIP-55 Display).
/// 2. Categorization applies the core's `is_gas_profitable` predicate ONCE
///    (the same `MIN_PROFIT_NET` floor the FFI batch fan-out applies: a net
///    at or above the floor submits; below counts gas-unprofitable). A
///    failure payload becomes a sim-fail row.
///    Python receives the verdict as a `kind` string only — it never reads
///    the threshold value.
/// 3. The boundary extracts the payload's primitive fields into the crate
///    `PayloadRow`, and the crate-owned join reassembles the core row, so a
///    submit row is the exact shape the submit seam consumes.
///
/// Args:
///     payloads: list of payload dicts (one per inline-sim entry).
///     engine: the `PyArbEngine` (the typed-hop resolver — the same
///         engine the payload-producing result batch came from).
///     `executor_address`: the session executor contract (the join stamps it
///         identically on every row, like the batch arm's join does).
///
/// Returns:
///     `PayloadOutcome` — the merged record set Python renders (submit rows,
///     unprofitable tally, sim-fail rows, `path_infos`, captured swaps are
///     empty on the payload arm: the engine's inspector capture rides the
///     render dict, not a `CapturedSwap` vector).
///
/// # Errors
///
/// `ValueError`: a payload dict is missing a required field, the calldata is
/// not bytes, or a `path_id` is not registered in the engine (the pool-key
/// derivation needs the typed hops).
#[pyfunction]
#[pyo3(signature = (payloads, engine, executor_address))]
pub fn merge_payload_results_py(
    py: Python<'_>,
    payloads: &Bound<'_, PyList>,
    engine: &crate::bot::engine::PyArbEngine,
    executor_address: &str,
) -> PyResult<PyPayloadOutcome> {
    let executor = crate::address_utils::parse_address(executor_address)
        .map_err(|e| PyValueError::new_err(format!("Invalid executor address: {e}")))?;

    // Boundary extraction only: each payload dict → the crate's `PayloadRow`.
    // The join + categorization policy is crate-owned
    // (`degenbot_batch_executor::assembly::merge_payload_results`) — the ONE
    // home the core batch executor's stage 1 and this seam both drive.
    let rows: Vec<PayloadRow> = payloads
        .iter()
        .map(|item| {
            let entry = item
                .cast::<PyDict>()
                .map_err(|_| PyValueError::new_err("payloads must be a list of payload dicts"))?;
            payload_row_from_dict(entry)
        })
        .collect::<PyResult<Vec<_>>>()?;

    // Resolve phase (GIL-held): one registered PathInfo per DISTINCT payload
    // path_id through the engine projection. A resolve miss here is decision
    // (a)'s loud-abort arm — a payload row is engine-born (the engine
    // simulated the path inline), so a miss evidences batch/registry
    // divergence and aborts — surfacing as the legacy `ValueError` (the
    // pre-cut-over driver's contract), with the crate merge's message.
    let mut resolved: HashMap<u64, PathInfo> = HashMap::with_capacity(rows.len());
    for row in &rows {
        if resolved.contains_key(&row.path_id) {
            continue;
        }
        let info = engine
            .path_info_for_core(py, row.path_id)
            .and_then(std::result::Result::ok)
            .ok_or_else(|| {
                PyValueError::new_err(format!(
                    "path_id {} is not registered in this engine; \
                     the payload pool keys cannot be derived",
                    row.path_id
                ))
            })?;
        resolved.insert(row.path_id, info);
    }

    // The join + categorization run crate-side over the resolved map.
    let merged = merge_payload_results(&rows, &ResolvedPaths { map: &resolved }, executor)
        .map_err(|e| PyValueError::new_err(e.detail))?;

    // The [profit]-render path_info dicts — the ONE serializer shape
    // (`path_info_to_py_dict`) both outcomes' path_infos emit, in
    // first-appearance order over DISTINCT path ids.
    let path_info_dicts: Vec<(u64, Py<PyDict>)> = merged
        .path_infos
        .iter()
        .map(|(path_id, info)| Ok((*path_id, path_info_to_py_dict(py, info)?.unbind())))
        .collect::<PyResult<Vec<_>>>()?;
    // Python switches its render branch on `kind` and reads nothing else —
    // the threshold comparison ran crate-side.
    let verdicts = merged
        .verdicts
        .iter()
        .map(|(path_id, arm)| {
            let kind = match arm {
                PayloadArm::Submit => "submit",
                PayloadArm::Unprofitable => "unprofitable",
            };
            PyPayloadVerdict::wrap(py, *path_id, kind)
        })
        .collect();

    Ok(PyPayloadOutcome {
        candidates: merged.submits,
        unprofitable_count: merged.unprofitable_count,
        failures: merged.failures,
        path_info_dicts,
        verdicts,
    })
}

/// Extract a required u64 field off one payload dict (seam helper).
fn required_u64(entry: &Bound<'_, PyDict>, key: &str) -> PyResult<u64> {
    entry
        .get_item(key)?
        .ok_or_else(|| PyValueError::new_err(format!("payload dict missing '{key}'")))?
        .extract()
        .map_err(|_| PyValueError::new_err(format!("payload field '{key}' must be an int")))
}

/// Extract one payload dict → the crate's `PayloadRow` — the boundary
/// translation (field names + Python types) only. The `access_list` rows the
/// serializer emitted (the EIP-2930 JSON shape) are parsed back into the
/// alloy `AccessList` so the crate join stamps the row exactly like the FFI
/// batch survivor.
pub(crate) fn payload_row_from_dict(entry: &Bound<'_, PyDict>) -> PyResult<PayloadRow> {
    let path_id = required_u64(entry, "path_id")?;
    let gross_obj = entry
        .get_item("gross_profit")?
        .ok_or_else(|| PyValueError::new_err("payload dict missing 'gross_profit'"))?;
    let gross_profit: alloy::primitives::U256 =
        crate::conversion::alloy::extract_python_u256(&gross_obj)?;
    let net_obj = entry
        .get_item("net_profit")?
        .ok_or_else(|| PyValueError::new_err("payload dict missing 'net_profit'"))?;
    let net_profit: alloy::primitives::U256 =
        crate::conversion::alloy::extract_python_u256(&net_obj)?;
    let calldata = entry
        .get_item("execute_calldata")?
        .ok_or_else(|| PyValueError::new_err("payload dict missing 'execute_calldata'"))?;
    let calldata_bytes: &[u8] = calldata
        .cast::<PyBytes>()
        .map_err(|_| PyValueError::new_err("payload field 'execute_calldata' must be bytes"))?
        .as_bytes();

    let access_list = match entry.get_item("access_list")? {
        Some(v) if !v.is_none() => Some(parse_payload_access_list(&v)?),
        _ => None,
    };

    let failure = match entry.get_item("failure")? {
        Some(fobj) if !fobj.is_none() => {
            let fdict = fobj
                .cast::<PyDict>()
                .map_err(|_| PyValueError::new_err("payload field 'failure' must be a dict"))?;
            Some(payload_failure_from_dict(fdict)?)
        }
        _ => None,
    };

    Ok(PayloadRow {
        path_id,
        gross_profit,
        net_profit,
        gas_used: required_u64(entry, "gas_used")?,
        priority_fee: required_u128(entry, "priority_fee")?,
        base_fee_next: required_u128(entry, "base_fee_next")?,
        execute_calldata: alloy::primitives::Bytes::copy_from_slice(calldata_bytes),
        access_list,
        failure,
    })
}

/// Parse the payload's access-list rows (the EIP-2930 JSON shape
/// `result_channel` emits: [{`address`, `storageKeys`: ["0x…32-byte hex", …]}, …])
/// into the alloy `AccessList` (the same decode the submission seam's
/// `parse_access_list` applies to `PySubmitCandidate`'s optional rows).
fn parse_payload_access_list(v: &Bound<'_, PyAny>) -> PyResult<alloy::rpc::types::AccessList> {
    let entries = v
        .try_iter()?
        .map(|row_any| {
            let row_ob = row_any?;
            let row = row_ob
                .cast::<PyDict>()
                .map_err(|_| PyValueError::new_err("access-list row must be a dict"))?;
            let addr_str: String = row
                .get_item("address")?
                .ok_or_else(|| PyValueError::new_err("access-list row missing 'address'"))?
                .extract()?;
            let address: alloy::primitives::Address =
                degenbot_core::address_utils::parse_address(&addr_str)
                    .map_err(|e| PyValueError::new_err(format!("bad access-list address: {e}")))?;
            let mut storage_keys: Vec<alloy::primitives::FixedBytes<32>> = Vec::new();
            if let Some(keys) = row.get_item("storageKeys")? {
                if !keys.is_none() {
                    for key in keys.try_iter()? {
                        let key = key.map_err(|_| {
                            PyValueError::new_err("storageKeys must be a list of hex strings")
                        })?;
                        let hex: String = key.extract().map_err(|_| {
                            PyValueError::new_err("storage key must be a hex string")
                        })?;
                        let bytes = alloy::primitives::hex::decode(&hex)
                            .map_err(|e| PyValueError::new_err(format!("bad storage key: {e}")))?;
                        let fb = alloy::primitives::FixedBytes::<32>::try_from(bytes.as_slice())
                            .map_err(|_| PyValueError::new_err("storage key must be 32 bytes"))?;
                        storage_keys.push(fb);
                    }
                }
            }
            Ok::<_, PyErr>(alloy::rpc::types::AccessListItem {
                address,
                storage_keys,
            })
        })
        .collect::<PyResult<Vec<_>>>()?;
    Ok(alloy::rpc::types::AccessList(entries))
}

/// Extract a required u128 field off one payload dict (seam helper).
fn required_u128(entry: &Bound<'_, PyDict>, key: &str) -> PyResult<u128> {
    entry
        .get_item(key)?
        .ok_or_else(|| PyValueError::new_err(format!("payload dict missing '{key}'")))?
        .extract()
        .map_err(|_| PyValueError::new_err(format!("payload field '{key}' must be an int")))
}

/// Build the crate's payload-failure record from the payload's failure
/// sub-dict — the FFI failures row keys (`bucket` / `fail_index` /
/// `revert_data`; the path id rides on the row). The revert bytes stay
/// `Bytes` here; the Python-facing hex string renders at the getter.
fn payload_failure_from_dict(failure: &Bound<'_, PyDict>) -> PyResult<PayloadFailure> {
    let bucket = match failure.get_item("bucket")? {
        Some(v) if !v.is_none() => v.extract::<String>()?,
        _ => "inline-fail".to_string(),
    };
    let fail_index = match failure.get_item("fail_index")? {
        Some(v) if !v.is_none() => Some(v.extract::<usize>().map_err(|_| {
            PyValueError::new_err("payload field 'fail_index' must be an int or null")
        })?),
        _ => None,
    };
    let revert_data = match failure.get_item("revert_data")? {
        Some(v) if !v.is_none() => {
            if let Ok(s) = v.extract::<String>() {
                let bytes =
                    alloy::primitives::hex::decode(s.trim_start_matches("0x")).map_err(|e| {
                        PyValueError::new_err(format!(
                            "payload field 'revert_data' is not hex: {e}"
                        ))
                    })?;
                alloy::primitives::Bytes::from(bytes)
            } else {
                let bytes: &[u8] = v.extract().map_err(|_| {
                    PyValueError::new_err("payload field 'revert_data' must be hex str or bytes")
                })?;
                alloy::primitives::Bytes::copy_from_slice(bytes)
            }
        }
        _ => alloy::primitives::Bytes::new(),
    };
    Ok(PayloadFailure {
        bucket,
        fail_index,
        revert_data,
    })
}

/// The per-entry categorization verdict — Python switches its render
/// branch on kind and reads nothing else. (submit / unprofitable; failure
/// entries surface through the failures rows, not a verdict.)
impl PyPayloadVerdict {
    fn wrap(py: Python<'_>, path_id: u64, kind: &'static str) -> Py<PyPayloadVerdict> {
        #[expect(clippy::unwrap_used)] // PyResult from Bound::new for a plain
        // pyclass cannot fail (no __new__ override, no GC-tracked fields).
        Py::new(py, PyPayloadVerdict { path_id, kind }).unwrap()
    }
}
#[pyclass(name = "PayloadVerdict", module = "degenbot._ffi.simulation")]
pub struct PyPayloadVerdict {
    /// The payload's path id.
    pub(crate) path_id: u64,
    /// "submit" | "unprofitable".
    pub(crate) kind: &'static str,
}

#[pymethods]
impl PyPayloadVerdict {
    /// The path id the verdict belongs to.
    #[getter]
    fn path_id(&self) -> u64 {
        self.path_id
    }

    /// "submit" | "unprofitable" — the arm tag (the threshold comparison ran
    /// Rust-side; Python renders the branch, it does not re-apply the rule).
    #[getter]
    fn kind(&self) -> &'static str {
        self.kind
    }
}

/// The merged inline-sim payload record set, built by
/// `merge_payload_results_py`. Rust owns every policy fact; Python renders +
/// stitches these getters into the merged outcome view.
#[pyclass(name = "PayloadOutcome", module = "degenbot._ffi.simulation")]
pub struct PyPayloadOutcome {
    /// The joined submit rows (the element type `dispatch_and_submit`
    /// re-extracts unchanged).
    pub(crate) candidates: Vec<SubmitCandidate>,
    /// Valid sims below the threshold (Rust-categorized).
    pub(crate) unprofitable_count: usize,
    /// The [sim-fail] rows (FFI failures-row shape).
    pub(crate) failures: Vec<(u64, PayloadFailure)>,
    /// The [profit]-render `path_info` dicts (`path_info_to_py_dict` shape).
    pub(crate) path_info_dicts: Vec<(u64, Py<PyDict>)>,
    /// The per-entry arm verdicts (`path_id` + `kind`).
    pub(crate) verdicts: Vec<Py<PyPayloadVerdict>>,
}

#[pymethods]
impl PyPayloadOutcome {
    /// The submit rows — `list[SubmitCandidate]`, the `gas_profitable`
    /// handoff shape (each element re-extracts as a `PySubmitCandidate`).
    #[getter]
    fn candidates<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for c in &self.candidates {
            let bound = Bound::new(
                py,
                crate::submission::submit::PySubmitCandidate { inner: c.clone() },
            )?;
            list.append(bound)?;
        }
        Ok(list)
    }

    /// Valid sims whose net profit fell below the threshold — the merged
    /// outcome's `gas_unprofitable_count` slice.
    #[getter]
    fn unprofitable_count(&self) -> usize {
        self.unprofitable_count
    }

    /// The [sim-fail] rows — list[dict] with the FFI failures row keys
    /// (`path_id` / `bucket` / `fail_index` / `revert_data`).
    #[getter]
    fn failures<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for (path_id, failure) in &self.failures {
            let dict = PyDict::new(py);
            dict.set_item("path_id", path_id)?;
            dict.set_item("bucket", &failure.bucket)?;
            match failure.fail_index {
                Some(idx) => dict.set_item("fail_index", idx)?,
                None => dict.set_item("fail_index", py.None())?,
            }
            // The revert bytes render as plain hex — the inline-sim payload
            // arm's historical shape (empty bytes → the empty string).
            dict.set_item(
                "revert_data",
                alloy::primitives::hex::encode(&failure.revert_data),
            )?;
            list.append(dict)?;
        }
        Ok(list)
    }

    /// `{path_id: path_info dict}` — the [profit]-render source (the SAME
    /// one-serializer shape the batch seam's `path_info_dict()` emits).
    #[getter]
    fn path_infos<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (pid, info) in &self.path_info_dicts {
            dict.set_item(pid, info.bind(py))?;
        }
        Ok(dict)
    }

    /// The per-entry verdicts — `list[PayloadVerdict]` (`path_id` + `kind`).
    #[getter]
    fn verdicts<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for v in &self.verdicts {
            list.append(v.bind(py))?;
        }
        Ok(list)
    }
}
