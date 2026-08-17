//! `PyO3` seam for the `degenbot_execution` seam crate (feature = `"execution"`).
//!
//! ADR-025 lift: adapt an arbitrary Python callable (`SolveResult -> bytes`)
//! into the core [`PayloadComposer`] / [`ExecutionStrategy`] seam — the
//! Polars-`map_elements` model (Rust holds a `Py<PyAny>` and calls back under
//! the GIL). Rust keeps ownership of the seam; Python supplies the Encode blob
//! against the caller's *own* contract (ADR-025 D2/D4).
//!
//! Exposed to Python as `degenbot._ffi.execution`:
//!
//! - [`PySolveResult`] — the typed solve-result **view** (ADR-025 D4): path id,
//!   hop count, the integer fixed-point amounts (`optimal_input` /
//!   `hop_outputs` / `consumed_inputs` / `net_profit`), and per-hop
//!   descriptors (family + addresses + direction). A Python compose callable
//!   reads these to build calldata.
//! - [`PyPayloadComposer`] — wraps a `result -> bytes` callable into a core
//!   [`PayloadComposer`] (which, via the seam's blanket impl, is also an
//!   [`ExecutionStrategy`]). When `compose` runs, Rust builds the
//!   [`SolveResult`] view, calls the Python callable under the GIL, and takes
//!   back the payload `bytes`.
//! - [`abi_encode_call`] — a thin `degenbot.abi`-backed helper so a Python user
//!   can ABI-encode a single call against their own contract from inside their
//!   compose callable.
//!
//! **Guardrails** (ADR-025): pyo3 lives ONLY here (in `degenbot-python` —
//! `just check-no-pyo3-in-cores` stays green); this is a *thin translate*, no
//! strategy logic core-side; nothing is threaded into the canonical
//! `dispatch_profitable_*` fan-out (the wall, D3) — `PyPayloadComposer` is the
//! foreign-contract path, adopted by a user directly, never used to re-derive
//! the canonical `cmd_executor` bundle.

use alloy::primitives::{Bytes, U256};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyModule};

use degenbot_execution::solve_result::{HopDescriptor, HopFamily};
use degenbot_execution::{ComposeError, ComposerInputs, PayloadComposer, SolveResult};
use degenbot_executor::composers::PathInfo;

use crate::conversion::alloy::u256_to_py;

// ═══════════════════════════════════════════════════════════════════════════
// PySolveResult — the solve-result view (ADR-025 D4)
// ═══════════════════════════════════════════════════════════════════════════

/// `PySolveResult` — the typed solve-result **view** passed to a Python
/// compose callable (ADR-025 D4).
///
/// Carries the path id, hop count, the integer fixed-point amounts
/// (`optimal_input` / `hop_outputs` / `consumed_inputs` / `net_profit`,
/// all u256 integers — never floats), and the per-hop descriptors. Constructed
/// by [`PyPayloadComposer`] from the core seam's inputs; a Python callable
/// reads it (attribute access) to build calldata for its own contract.
#[pyclass(name = "SolveResult", module = "degenbot._ffi.execution")]
pub struct PySolveResult {
    inner: SolveResult,
}

impl PySolveResult {
    /// A fresh [`Py<Self>`] wrapping a [`SolveResult`]. Rust builds the view;
    /// Python never constructs one directly (the view is produced by
    /// introspection on the solve path, not hand-assembled).
    ///
    /// # Errors
    ///
    /// Returns `Err` only if `Py::new` fails to allocate (essentially
    /// infallible).
    pub fn wrap(py: Python<'_>, inner: SolveResult) -> PyResult<Py<Self>> {
        Py::new(py, Self { inner })
    }

    /// Project the view from the canonical solver intake (ADR-025 D4's "one
    /// genuinely new surface"): [`SolvePathResult`] amounts + [`PathInfo`] hop
    /// descriptors → the typed [`SolveResult`] view a Python compose callable
    /// (or any consumer) reads — [`SolveResult::from_solve_path`].
    ///
    /// Thin projection only: no business logic, no changes to the canonical
    /// `dispatch_profitable_*` output (still `execute_calldata`; the wall).
    ///
    /// # Errors
    ///
    /// Returns `Err` only if `Py::new` fails to allocate (essentially
    /// infallible).
    pub fn from_solve_path(
        py: Python<'_>,
        path_id: u64,
        result: &degenbot_solvers::mixed::SolvePathResult,
        path: &PathInfo,
    ) -> PyResult<Py<Self>> {
        Self::wrap(py, SolveResult::from_solve_path(path_id, result, path))
    }
}

#[pymethods]
impl PySolveResult {
    /// The path id (`path_id`).
    #[getter]
    fn path_id(&self) -> u64 {
        self.inner.path_id
    }

    /// Number of hops in the path (`hop_count`).
    #[getter]
    fn hop_count(&self) -> usize {
        self.inner.hop_count
    }

    /// The flash input amount (`optimal_input`, uint256 integer).
    #[getter]
    fn optimal_input<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        u256_to_py(py, &self.inner.optimal_input)
    }

    /// Per-hop output amounts (`hop_outputs`, list of uint256 integers).
    #[getter]
    fn hop_outputs<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let list = PyList::empty(py);
        for v in &self.inner.hop_outputs {
            list.append(u256_to_py(py, v)?)?;
        }
        Ok(list.into_any())
    }

    /// Per-hop consumed input amounts (`consumed_inputs`, list of uint256
    /// integers). The CL-clamp swap-in matters: an over-fed CL hop is reduced
    /// to `input_consumed − 1`.
    #[getter]
    fn consumed_inputs<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let list = PyList::empty(py);
        for v in &self.inner.consumed_inputs {
            list.append(u256_to_py(py, v)?)?;
        }
        Ok(list.into_any())
    }

    /// The net profit (`net_profit`, uint256 integer) — `final_output −
    /// consumed_inputs[0]`.
    #[getter]
    fn net_profit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        u256_to_py(py, &self.inner.net_profit)
    }

    /// Per-hop descriptors — a list of dicts, one per hop:
    /// `{family, pool_address, token0, token1, zfo, v4_pool_id}`.
    #[getter]
    fn hop_descriptors<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let list = PyList::empty(py);
        for d in &self.inner.hop_descriptors {
            list.append(hop_descriptor_to_dict(py, d)?)?;
        }
        Ok(list.into_any())
    }
}

/// Build a descriptor dict for one hop (`family` tag + addresses + direction).
fn hop_descriptor_to_dict<'py>(py: Python<'py>, d: &HopDescriptor) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item(
        "family",
        match d.family {
            HopFamily::V2 => "V2",
            HopFamily::V3 => "V3",
            HopFamily::V4 => "V4",
        },
    )?;
    dict.set_item("pool_address", d.pool_address.to_checksum(None))?;
    dict.set_item("token0", d.token0.to_checksum(None))?;
    dict.set_item("token1", d.token1.to_checksum(None))?;
    dict.set_item("zfo", d.zfo)?;
    dict.set_item("v4_pool_id", d.v4_pool_id.as_deref())?;
    Ok(dict)
}

// ═══════════════════════════════════════════════════════════════════════════
// PyPayloadComposer — lift a Python callable into the PayloadComposer seam
// ═══════════════════════════════════════════════════════════════════════════

/// `PyPayloadComposer` — wrap an arbitrary Python callable
/// (`result: PySolveResult -> bytes`) into the core [`PayloadComposer`] seam.
///
/// Construct with a Python callable:
///
/// ```python
/// def my_composer(result):
///     return abi_encode_call("(bytes32,uint256)", ["0x…", result.optimal_input])
///
/// composer = PyPayloadComposer(my_composer)
/// ```
///
/// Rust drives `compose` (under the GIL it builds the [`SolveResult`] view,
/// calls the Python callable, and takes back the payload `bytes`). Because the
/// seam provides a blanket `PayloadComposer -> ExecutionStrategy` impl, a
/// `PyPayloadComposer` is a full `ExecutionStrategy` (built-in Probe/Assess/Fee
/// defaults) that a foreign searcher can adopt directly. Nothing here is wired
/// into the canonical `dispatch_profitable_*` fan-out (ADR-025 D3).
///
/// The callable's return must be `bytes`/`bytearray` (the payload to submit to
/// the composer's own contract).
#[pyclass(name = "PayloadComposer", module = "degenbot._ffi.execution", subclass)]
#[derive(Debug)]
pub struct PyPayloadComposer {
    /// The held Python callable. Rust re-enters the GIL (`Python::attach`)
    /// to invoke it with a [`PySolveResult`], mirroring the existing
    /// GIL-held-callable patterns in this crate (`PyBotIo` / subscriber
    /// callbacks).
    callback: Py<PyAny>,
}

#[pymethods]
impl PyPayloadComposer {
    /// Construct from a Python callable `result -> bytes`.
    ///
    /// # Errors
    ///
    /// `TypeError` when `callback` is not callable.
    #[new]
    fn new(callback: Py<PyAny>) -> PyResult<Self> {
        // Reject non-callables early (clearer than failing inside `compose`).
        Python::attach(|py| {
            if !callback.bind(py).is_callable() {
                return Err(PyTypeError::new_err(
                    "PayloadComposer expects a callable `result -> bytes`",
                ));
            }
            Ok(())
        })?;
        Ok(Self { callback })
    }
}

impl PayloadComposer for PyPayloadComposer {
    fn compose(&self, path: &PathInfo, inputs: &ComposerInputs<'_>) -> Result<Bytes, ComposeError> {
        let result = solve_result_from_path_inputs(path, inputs);
        Python::attach(|py| {
            let view = PySolveResult::wrap(py, result)?;
            let returned = self.callback.bind(py).call1((view,))?;
            let vec: Vec<u8> = returned.extract().map_err(|_| {
                pyo3::exceptions::PyValueError::new_err(
                    "compose callback must return bytes/bytearray (payload for the contract)",
                )
            })?;
            Ok(Bytes::from(vec))
        })
        .map_err(|err: PyErr| ComposeError::encode(err.to_string()))
    }
}

/// Build a [`SolveResult`] view from the seam's `(path, inputs)` intake.
///
/// The `PayloadComposer::compose` seam carries the solver-driven amounts as
/// `ComposerInputs` (u128) + the hop descriptors via `PathInfo`, but not the
/// solver's `path_id`. The view path (`ExecutionStrategy::compose_view`) is the
/// one that carries a full `SolveResult` including `path_id`; here the encode
/// seam doesn't need it, so it is projected as `0`. `net_profit` is derived as
/// `final_output − consumed_inputs[0]` (the same identity the seam documents).
fn solve_result_from_path_inputs(path: &PathInfo, inputs: &ComposerInputs<'_>) -> SolveResult {
    let final_output = inputs.hop_outputs.last().copied().unwrap_or(0);
    let consumed_0 = inputs.consumed_inputs.first().copied().unwrap_or(0);
    let net_profit = final_output.saturating_sub(consumed_0);
    SolveResult {
        path_id: 0,
        hop_count: path.hops.len(),
        optimal_input: U256::from(inputs.optimal_input),
        hop_outputs: inputs.hop_outputs.iter().map(|v| U256::from(*v)).collect(),
        consumed_inputs: inputs
            .consumed_inputs
            .iter()
            .map(|v| U256::from(*v))
            .collect(),
        net_profit: U256::from(net_profit),
        hop_descriptors: path.hops.iter().map(HopDescriptor::from_hop_info).collect(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// abi_encode_call — degenbot.abi-backed helper for a user's own contract
// ═══════════════════════════════════════════════════════════════════════════

/// `abi_encode_call(signature, values) -> bytes`
///
/// A `degenbot.abi`-backed helper so a Python user can ABI-encode a **call**
/// against their own contract from inside a compose callable. `signature` is a
/// Solidity function signature string (`"transfer(address,uint256)"`);
/// `values` is the argument list consumed left-to-right.
///
/// Returns the full calldata: the 4-byte function selector (from the parsed,
/// canonical signature) followed by the ABI-encoded arguments. The argument
/// encoding delegates to the crate's canonical ABI encoder (via
/// `degenbot._ffi.abi.encode`); this wrapper only adds the selector prefix and
/// the signature-string spelling — it is not a second ABI implementation.
///
/// # Errors
///
/// `ValueError` when the signature cannot be parsed or the values cannot be
/// encoded.
#[pyfunction]
fn abi_encode_call<'py>(
    py: Python<'py>,
    signature: &str,
    values: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyBytes>> {
    let parsed = degenbot_abi::signature_parser::parse_signature(signature)
        .map_err(|e| PyValueError::new_err(format!("bad signature: {e}")))?;
    if !parsed.outputs.is_empty() {
        return Err(PyValueError::new_err(
            "signature must not declare a `returns` clause (calldata has no outputs)",
        ));
    }
    // 4-byte function selector = keccak256(canonical-signature)[..4].
    let mut selector = [0u8; 4];
    selector.copy_from_slice(
        &alloy::primitives::keccak256(parsed.to_signature_string().as_bytes())[..4],
    );
    // Delegate the argument encoding to the canonical `degenbot.abi.encode`
    // (types + values), which already handles Python→AbiValue conversion.
    let types: Vec<String> = parsed.inputs.iter().map(ToString::to_string).collect();
    let types_py = PyList::new(py, &types)?;
    let abi_mod = py.import("degenbot._ffi.abi")?;
    let encode = abi_mod.getattr("encode")?;
    let encoded = encode.call1((types_py, values))?;
    let encoded: Vec<u8> = encoded
        .extract()
        .map_err(|_| PyValueError::new_err("degenbot.abi.encode did not return bytes"))?;
    let mut calldata = Vec::with_capacity(4 + encoded.len());
    calldata.extend_from_slice(&selector);
    calldata.extend_from_slice(&encoded);
    Ok(PyBytes::new(py, &calldata))
}

// ═══════════════════════════════════════════════════════════════════════════
// Module registration
// ═══════════════════════════════════════════════════════════════════════════

/// Register the execution-seam classes/functions on `m` (feature =
/// `"execution"`) as `degenbot._ffi.execution`.
///
/// # Errors
///
/// Returns a [`PyErr`] if any `add_class`/`add_function` call fails (e.g. a
/// name collision); propagated unchanged to the `#[pymodule]` caller.
pub fn add_execution_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.execution")?;
    submod.add_class::<PySolveResult>()?;
    submod.add_class::<PyPayloadComposer>()?;
    submod.add_function(wrap_pyfunction!(abi_encode_call, &submod)?)?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.execution", &submod)?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::Address;
    use degenbot_executor::composers::{HopInfo, V2HopInfo};

    const WETH: Address = alloy::primitives::address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    const USDC: Address = alloy::primitives::address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");

    fn sample_path() -> PathInfo {
        PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: alloy::primitives::address!(
                    "1111111111111111111111111111111111111111"
                ),
                token0_address: WETH,
                token1_address: USDC,
                fee: 30,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: alloy::primitives::address!(
                    "2222222222222222222222222222222222222222"
                ),
                token0_address: USDC,
                token1_address: WETH,
                fee: 30,
                zfo: true,
            }),
        ])
    }

    #[test]
    fn compose_calls_python_callable_with_view_and_takes_bytes() {
        Python::attach(|py| {
            // A Python callable that reads the view and returns fixed bytes.
            let callback = py
                .eval(
                    c"lambda result: bytes([0xCA, 0xFE]) \
                     + bytes([result.path_id, result.hop_count]) \
                     + result.consumed_inputs[0].to_bytes(16, 'big')",
                    None,
                    None,
                )
                .unwrap();
            let composer = PyPayloadComposer::new(callback.unbind()).unwrap();
            let path = sample_path();
            let outputs = [1_000_000_000_000_000_000u128, 1_000_000_000_000_000_001u128];
            let consumed = [1_000_000_000_000_000_000u128, 1_000_000_000_000_000_000u128];
            let inputs = ComposerInputs {
                optimal_input: outputs[0],
                hop_outputs: &outputs,
                consumed_inputs: &consumed,
                opts: degenbot_execution::ComposeOptions,
            };
            let bytes = composer
                .compose(&path, &inputs)
                .expect("compose should succeed");
            // prefix + path_id(0) + hop_count(2) + consumed_inputs[0] (16 bytes)
            assert_eq!(&bytes[..2], &[0xCA, 0xFE]);
            assert_eq!(bytes[2], 0);
            assert_eq!(bytes[3], 2);
            assert_eq!(
                bytes[4..].to_vec(),
                1_000_000_000_000_000_000u128.to_be_bytes().to_vec()
            );
        });
    }

    #[test]
    fn view_exposes_descriptors_and_amounts() {
        Python::attach(|py| {
            let callback = py
                .eval(
                    c"lambda result: bytes([len(result.hop_descriptors), \
                     result.hop_descriptors[0]['family'] == 'V2'])",
                    None,
                    None,
                )
                .unwrap();
            let composer = PyPayloadComposer::new(callback.unbind()).unwrap();
            let path = sample_path();
            let outputs = [100u128, 101u128];
            let consumed = [100u128, 100u128];
            let inputs = ComposerInputs {
                optimal_input: outputs[0],
                hop_outputs: &outputs,
                consumed_inputs: &consumed,
                opts: degenbot_execution::ComposeOptions,
            };
            let bytes = composer.compose(&path, &inputs).expect("compose");
            assert_eq!(bytes.to_vec(), vec![2, 1]);
        });
    }

    #[test]
    fn non_callable_rejected_at_construction() {
        Python::attach(|py| {
            let non_callable = py.eval(c"42", None, None).unwrap().unbind();
            let err = PyPayloadComposer::new(non_callable).unwrap_err();
            assert!(err.to_string().contains("callable"));
        });
    }

    #[test]
    fn view_projects_from_canonical_solve_result() {
        // V6PLQA: `PySolveResult::from_solve_path` projects the typed view from
        // the canonical `SolvePathResult` (amounts) + `PathInfo` (hop
        // descriptors) — the "one genuinely new surface" (ADR-025 D4).
        use degenbot_solvers::mixed::SolvePathResult;
        Python::attach(|py| {
            let result = SolvePathResult {
                optimal_input: U256::from(1_000_000_000_000_000_000u64),
                hop_outputs: vec![
                    U256::from(1_000_000_000_000_000_000u64),
                    U256::from(1_000_000_000_000_000_050u64),
                ],
                consumed_inputs: vec![U256::from(1_000_000_000_000_000_000u64); 2],
                profit: U256::from(50u64),
                ..Default::default()
            };

            let path = sample_path();
            let view = PySolveResult::from_solve_path(py, 7, &result, &path).unwrap();

            assert_eq!(
                view.bind(py)
                    .getattr("path_id")
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                7
            );
            assert_eq!(
                view.bind(py)
                    .getattr("hop_count")
                    .unwrap()
                    .extract::<usize>()
                    .unwrap(),
                2
            );
            assert_eq!(
                view.bind(py)
                    .getattr("optimal_input")
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                1_000_000_000_000_000_000u64
            );
            assert_eq!(
                view.bind(py)
                    .getattr("net_profit")
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                50
            );
            // amounts stay integer u256 (no float) — hop_outputs[1] round-trips.
            let outs: Vec<u64> = view
                .bind(py)
                .getattr("hop_outputs")
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(
                outs,
                vec![1_000_000_000_000_000_000u64, 1_000_000_000_000_000_050u64]
            );
            // hop descriptors render as dicts with the V2 family.
            let descs: Vec<pyo3::Py<PyAny>> = view
                .bind(py)
                .getattr("hop_descriptors")
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(descs.len(), 2);
        });
    }
}
