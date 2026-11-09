//! `PyO3` seam for the `degenbot_execution` seam crate (feature = `"execution"`).
//!
//! ADR-025 lift: adapt an arbitrary Python callable (`SolveResult -> bytes`)
//! into the core [`PayloadComposer`] / [`ExecutionAdapter`] seam — the
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
//!   [`PayloadComposer`] (encoding only: calldata, no target or value).
//! - [`PyPayloadAdapter`] — binds that callable to the complete transaction the
//!   production seam carries: `to`, an optional native `value`, declared probe
//!   reads, and a minimum net profit. It is a full core
//!   [`ExecutionAdapter`], so a Python consumer authors the same
//!   `{ to, data, value }` transaction a foreign Rust adapter does.
//! - [`abi_encode_call`] — a thin `degenbot.abi`-backed helper so a Python user
//!   can ABI-encode a single call against their own contract from inside their
//!   compose callable.
//! - [`UnsupportedPath`] — the typed **routine decline** a compose callable
//!   raises for a path its contract cannot encode. It is the Python spelling of
//!   [`degenbot_execution::ComposeError::Unsupported`]: the target-selection walk
//!   advances to the next target. Every other callback exception is that
//!   candidate's hard [`degenbot_execution::ComposeError::Encode`].
//!
//! **Guardrails** (ADR-025, ADR-098 D3): pyo3 lives ONLY here (in
//! `degenbot-python` — `just check-no-pyo3-in-cores` stays green); this is a
//! *thin translate*, no strategy logic core-side; nothing is threaded into the
//! canonical `dispatch_profitable_*` fan-out (the wall, D3). Python declares
//! the encoding, the target/value binding, the probe reads, and the minimum
//! profit — it does not acquire a simulation implementation, an assessment
//! interpreter, or a Python mirror of the built-in command composer. The
//! engine's typed `ExecutionObservation` and the custom-Assess hook stay Rust
//! types: Python states *policy data*, and the Rust seam interprets.

use alloy::dyn_abi::{DynSolType, DynSolValue, Specifier};
use alloy::json_abi::Function;
use alloy::primitives::{hex, Address, Bytes, B256, U256};
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};

use degenbot_execution::solve_result::{HopDescriptor, HopFamily};
use degenbot_execution::{
    case_hop_family, run_cases, AssessOptions, AssessRule, CaseAmounts, CaseFailure, CaseReport,
    ComposeError, ComposerInputs, ContractInterface, DeclarationError, ExecutionAdapter, Expected,
    ExpectedCall, ExpectedTransaction, FeePolicy, HopInfo, PathInfo, PayloadComposer, ProbeSpec,
    ProbeSpecs, SolveResult, TransactionRequest, V2HopInfo, V3HopInfo, V4HopInfo, ValidationCase,
    ValuePolicy,
};

use crate::conversion::alloy::{extract_python_u256, u256_to_py};

// ═══════════════════════════════════════════════════════════════════════════
// PySolveResult — the solve-result view (ADR-025 D4)
// ═══════════════════════════════════════════════════════════════════════════

/// `PySolveResult` — the typed solve-result **view** passed to a Python
/// compose callable (ADR-025 D4).
///
/// Carries the path id, hop count, the integer fixed-point amounts
/// (`optimal_input` / `hop_outputs` / `consumed_inputs` / `net_profit`,
/// all u256 integers — never floats), and the per-hop descriptors. Constructed
/// by the Encode lift from the core seam's inputs; a Python callable reads it
/// (attribute access) to build calldata for its own contract.
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
        result: &degenbot_pools::solve::SolvePathResult,
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
    /// integers). The concentrated-liquidity-clamp swap-in matters: an over-fed concentrated liquidity hop is reduced
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
///
/// The `family` value is the registry's internal kebab tag, the one spelling
/// every internal family reader reports.
fn hop_descriptor_to_dict<'py>(py: Python<'py>, d: &HopDescriptor) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item(
        "family",
        match d.family {
            HopFamily::V2 => degenbot_pools::RegisteredPoolFamily::V2.tag(),
            HopFamily::V3 => degenbot_pools::RegisteredPoolFamily::V3.tag(),
            HopFamily::V4 => degenbot_pools::RegisteredPoolFamily::V4.tag(),
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
// Encode lift — the shared Python-callable invocation
// ═══════════════════════════════════════════════════════════════════════════

/// Wrap a held Python callable and require it to be callable at construction.
///
/// # Errors
///
/// `TypeError` when `callback` is not callable.
fn require_callable(callback: &Py<PyAny>, expected: &str) -> PyResult<()> {
    Python::attach(|py| {
        if callback.bind(py).is_callable() {
            Ok(())
        } else {
            Err(PyTypeError::new_err(format!(
                "{expected} expects a callable `result -> bytes`"
            )))
        }
    })
}

// The compose callable's routine path-decline signal.
//
// A callback raises `UnsupportedPath` for a path its own contract cannot encode —
// the explicit spelling of `ComposeError::Unsupported`, which is a CAPABILITY
// decision the adapter makes, not a verdict on its declaration. The bridge
// translates it (and only it) into that arm, so the first-compatible walk
// examines the next target instead of failing the candidate (ADR-098 D5,
// ADR-100 D5).
//
// It is deliberately NOT a validation failure: a declaration violation is
// `ComposeError::Validation`, produced by the binding's own check, never by the
// callable. Every other exception a callback raises — whatever its type,
// including anything raised from deeper inside the callback — remains a hard
// `ComposeError::Encode`, so a broken encoder is reported, never excused as a
// decline. Subclasses count: the bridge classifies by `is_instance_of`, the way a
// Python `except UnsupportedPath` would.
//
// Normal Python exception semantics apply: the decline's detail is the
// exception's own `str()` (empty for a bare `raise UnsupportedPath`).
create_exception!(
    degenbot._ffi.execution,
    UnsupportedPath,
    PyException,
    "The adapter declined this path: a routine capability refusal (ComposeError::Unsupported), not an interface-validation failure, so the target-selection walk examines the next target. Every other exception a compose callable raises is a hard encode failure."
);

/// Classify a compose callable's Python failure as the seam's typed encode error.
///
/// [`UnsupportedPath`] is the one routine decline ([`ComposeError::unsupported`],
/// carrying the exception's own message); every other exception is that
/// candidate's unrecoverable encode failure ([`ComposeError::encode`]).
fn compose_error(py: Python<'_>, err: &PyErr) -> ComposeError {
    if !err.is_instance_of::<UnsupportedPath>(py) {
        return ComposeError::encode(err.to_string());
    }
    let detail = err.value(py).str().map_or_else(
        |_| err.to_string(),
        |text| text.to_string_lossy().into_owned(),
    );
    ComposeError::unsupported(detail)
}

/// Call the held Python callable with the typed solve-result view and take back
/// the calldata bytes.
///
/// The view is projected from the seam's `(path, inputs)` intake; the callback's
/// return is the payload for the composer's own contract. A callback that raises
/// [`UnsupportedPath`] declines the path ([`ComposeError::Unsupported`]); any
/// other exception is a hard [`ComposeError::Encode`].
fn compose_with_callable(
    callback: &Py<PyAny>,
    path: &PathInfo,
    inputs: &ComposerInputs<'_>,
) -> Result<Bytes, ComposeError> {
    let result = solve_result_from_path_inputs(path, inputs);
    Python::attach(|py| {
        let view = PySolveResult::wrap(py, result)?;
        let returned = callback.bind(py).call1((view,))?;
        let vec: Vec<u8> = returned.extract().map_err(|_| {
            PyValueError::new_err(
                "compose callback must return bytes/bytearray (calldata for the contract)",
            )
        })?;
        Ok(Bytes::from(vec))
    })
    .map_err(|err: PyErr| Python::attach(|py| compose_error(py, &err)))
}

/// Build a [`SolveResult`] view from the seam's `(path, inputs)` intake.
///
/// The `PayloadComposer::compose` seam carries the solver-driven amounts as
/// `ComposerInputs` (u128) + the hop descriptors via `PathInfo`, but not the
/// solver's `path_id`. The view path (`ExecutionAdapter::compose_view`) is the
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
// PyPayloadComposer — lift a Python callable into the PayloadComposer seam
// ═══════════════════════════════════════════════════════════════════════════

/// `PyPayloadComposer` — wrap an arbitrary Python callable
/// (`result: PySolveResult -> bytes`) into the core [`PayloadComposer`] seam.
///
/// This is the **encoding-only** route: it produces calldata and nothing else,
/// so it carries no target and no native value. Use
/// [`PyPayloadAdapter`] when the transaction needs a target (and possibly a
/// `value`), which is what the production seam carries.
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
/// calls the Python callable, and takes back the payload `bytes`). Nothing here
/// is wired into the canonical `dispatch_profitable_*` fan-out (ADR-025 D3).
///
/// The callable's return must be `bytes`/`bytearray` (the calldata for its
/// own contract). A callable that cannot encode the path raises
/// [`UnsupportedPath`] — the adapter's routine capability decline, which the
/// target-selection walk advances past; any other exception it raises is that
/// candidate's hard `ComposeError::Encode`.
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
        require_callable(&callback, "PayloadComposer")?;
        Ok(Self { callback })
    }
}

impl PayloadComposer for PyPayloadComposer {
    fn compose(&self, path: &PathInfo, inputs: &ComposerInputs<'_>) -> Result<Bytes, ComposeError> {
        compose_with_callable(&self.callback, path, inputs)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PyPayloadAdapter — the complete transaction a Python consumer authors
// ═══════════════════════════════════════════════════════════════════════════

/// `PayloadAdapter` — a Python-composed foreign execution adapter: the Encode
/// callable bound to the transaction it executes.
///
/// The production seam's Encode result is one complete transaction
/// (`to` + `data` + native `value`). A Python consumer supplies the calldata
/// callable plus the binding calldata cannot carry:
///
/// ```python
/// adapter = PayloadAdapter(
///     compose_simple_executor,            # result -> bytes
///     "0x7777…7777",                      # the contract the call targets
///     value=0,                            # native value sent with the call (wei)
///     probes=[
///         # A contract-call read carries its COMPLETE calldata.
///         {"label": "WETH", "to": "0xC02a…Cc2", "data": balance_of_executor},
///         # A native-account read observes the account itself.
///         {"label": "ETH", "account": "0x7777…7777"},
///     ],
///     min_net_profit=0,
/// )
/// ```
///
/// It implements the full core `ExecutionAdapter`: Encode returns the bound
/// transaction, Probe is the declared read list, and Assess is the seam's
/// default `sum_of_deltas` gate over those readings with the declared minimum
/// net profit and the defaulted market-percentile fee. A custom assessment
/// interpreter is a Rust-side override — Python states policy data, it does not
/// run a second assessment engine.
///
/// An optional `interface` declaration attaches the core's compiled
/// `ContractInterface` to this binding: every encode is then checked against it
/// (declared selector, decodable arguments, declared target and native value),
/// and `check_cases` can compare explicit path/amount literals against the
/// transaction it must produce. The declaration is decomposed by the binding
/// from standard ABI JSON plus optional selector/target/value policies; Python
/// authors no ABI parser. An adapter that declares nothing keeps the exact
/// encode path it has today.
///
/// The callable declines a path its contract cannot encode by raising
/// [`UnsupportedPath`]: that is the adapter's routine capability refusal
/// (`ComposeError::Unsupported`), not a verdict on the declaration, and the
/// target-selection walk advances to the next target. Every other exception it
/// raises is that candidate's hard `ComposeError::Encode`.
#[pyclass(name = "PayloadAdapter", module = "degenbot._ffi.execution", subclass)]
#[derive(Debug)]
pub struct PyPayloadAdapter {
    callback: Py<PyAny>,
    to: Address,
    value: U256,
    probes: ProbeSpecs,
    min_net_profit: U256,
    /// The compiled declaration this adapter's encodes are checked against,
    /// when the binding declared one. `None` is the undeclared default: no
    /// decode runs, no field is consulted, and the encode path is unchanged.
    interface: Option<ContractInterface>,
}

/// The sorted key set a contract-call probe declaration carries.
const PROBE_CALL_KEYS: [&str; 3] = ["data", "label", "to"];
/// The sorted key set a native-account probe declaration carries.
const PROBE_NATIVE_KEYS: [&str; 2] = ["account", "label"];

/// Parse the declared probe list, one dict per probe:
///
/// - `{"label": str, "to": str, "data": bytes}` — a contract-call read carrying
///   its COMPLETE calldata (selector plus ABI arguments);
/// - `{"label": str, "account": str}` — the declared account's own native (ETH)
///   balance.
///
/// # Errors
///
/// `ValueError` when an entry is not a dict, its keys are not exactly one of the
/// two declaration shapes, an address cannot be parsed, or a call's `data` is
/// not `bytes` carrying at least the 4-byte selector.
fn parse_probes(probes: Option<&Bound<'_, PyAny>>) -> PyResult<ProbeSpecs> {
    let Some(probes) = probes else {
        return Ok(Vec::new());
    };
    if probes.is_none() {
        return Ok(Vec::new());
    }
    let mut specs = Vec::new();
    for entry in probes.try_iter()? {
        let entry = entry?;
        specs.push(parse_probe(&entry)?);
    }
    Ok(specs)
}

/// Parse one probe declaration dict into its neutral core form.
fn parse_probe(entry: &Bound<'_, PyAny>) -> PyResult<ProbeSpec> {
    let dict = entry.cast::<PyDict>().map_err(|_| {
        PyValueError::new_err(
            "each probe must be a dict: {'label', 'to', 'data'} for a contract call \
             or {'label', 'account'} for a native balance read",
        )
    })?;
    let keys = probe_keys(dict)?;
    let keys_sorted: Vec<&str> = keys.iter().map(String::as_str).collect();
    let is_call = PROBE_CALL_KEYS.as_slice() == keys_sorted.as_slice();
    let is_native = PROBE_NATIVE_KEYS.as_slice() == keys_sorted.as_slice();
    if !is_call && !is_native {
        return Err(PyValueError::new_err(format!(
            "a probe declares exactly one form: {{'label', 'to', 'data'}} for a contract \
             call or {{'label', 'account'}} for a native balance read (got {keys:?})"
        )));
    }
    let label: String = dict
        .get_item("label")?
        .ok_or_else(|| PyValueError::new_err("a probe needs a `label`"))?
        .extract()
        .map_err(|_| PyValueError::new_err("a probe `label` must be a str"))?;
    if is_call {
        let to = dict
            .get_item("to")?
            .ok_or_else(|| PyValueError::new_err("a probe call needs a `to`"))?
            .extract::<String>()
            .map_err(|_| PyValueError::new_err("a probe `to` must be an address string"))?;
        let to = crate::address_utils::parse_address(&to)
            .map_err(|err| PyValueError::new_err(err.to_string()))?;
        let data: Vec<u8> = dict
            .get_item("data")?
            .ok_or_else(|| PyValueError::new_err("a probe call needs `data`"))?
            .extract()
            .map_err(|_| PyValueError::new_err("a probe `data` must be bytes"))?;
        if data.len() < 4 {
            return Err(PyValueError::new_err(
                "a probe call's `data` must carry at least the 4-byte selector",
            ));
        }
        return Ok(ProbeSpec::call(label, to, data));
    }
    let account = dict
        .get_item("account")?
        .ok_or_else(|| PyValueError::new_err("a native balance probe needs an `account`"))?
        .extract::<String>()
        .map_err(|_| PyValueError::new_err("a probe `account` must be an address string"))?;
    let account = crate::address_utils::parse_address(&account)
        .map_err(|err| PyValueError::new_err(err.to_string()))?;
    Ok(ProbeSpec::native_balance(label, account))
}

/// The sorted key set of one probe declaration dict, so an unknown or missing
/// key fails loud instead of silently narrowing the declaration.
fn probe_keys(dict: &Bound<'_, PyDict>) -> PyResult<Vec<String>> {
    let mut keys = Vec::with_capacity(dict.len());
    for key in dict.keys() {
        keys.push(
            key.extract::<String>()
                .map_err(|_| PyValueError::new_err("probe dict keys must be str"))?,
        );
    }
    keys.sort();
    Ok(keys)
}

#[pymethods]
impl PyPayloadAdapter {
    /// Construct from a calldata callable plus its transaction binding.
    ///
    /// `to` is the contract the call targets; `value` is the native value sent
    /// with it (wei, zero for a flash-funded contract); `probes` is the declared
    /// pre/post read list, one dict per probe: `{"label", "to", "data"}` for a
    /// contract call carrying its complete calldata (selector plus ABI
    /// arguments) or `{"label", "account"}` for a native-account balance read;
    /// `min_net_profit` is the gate threshold in wei.
    ///
    /// `interface` is the optional declared interface, given as one dict:
    ///
    /// ```text
    /// {
    ///     "abi": [ ... ],                       # a standard ABI JSON array
    ///     "selectors": ["execute(uint256,...)"],  # optional signature allow-list
    ///     "value": {"kind": "zero"},             # optional native-value policy
    ///     "target": "0x7777…7777",               # optional; defaults to `to`
    /// }
    /// ```
    ///
    /// The binding — not the caller — serializes the `abi` array to ABI JSON
    /// text, parses the addresses, resolves the value policy, and stamps the
    /// target from its own `to`, because one binding encodes to one address. A
    /// declared `target` that disagrees with `to` is refused. Construction is
    /// pure data validation: it never invokes `callback`, never reads the chain,
    /// and never touches a provider, and it checks the declared probe list
    /// against the declaration (a probe reading this adapter's own contract must
    /// name a declared selector with decodable arguments).
    ///
    /// # Errors
    ///
    /// `TypeError` when `callback` is not callable; `ValueError` for an
    /// unparseable address, a malformed probe entry (including the retired
    /// `(label, address, selector)` triple, which cannot carry a call's
    /// arguments), a negative value, or an unusable `interface` declaration — a
    /// malformed ABI JSON array, a surface that declares no function, two
    /// functions sharing a selector, an allow-listed signature the ABI does not
    /// declare, an invalid value policy, a declared target that disagrees with
    /// `to`, or a declared probe to this adapter's own contract that the
    /// declaration does not describe.
    #[new]
    #[pyo3(signature = (
        callback,
        to,
        value = None,
        probes = None,
        min_net_profit = None,
        interface = None
    ))]
    fn new(
        callback: Py<PyAny>,
        to: &str,
        value: Option<&Bound<'_, PyAny>>,
        probes: Option<&Bound<'_, PyAny>>,
        min_net_profit: Option<&Bound<'_, PyAny>>,
        interface: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        require_callable(&callback, "PayloadAdapter")?;
        let to = crate::address_utils::parse_address(to)
            .map_err(|err| PyValueError::new_err(err.to_string()))?;
        let probes = parse_probes(probes)?;
        let interface = parse_interface(interface, to, &probes)?;
        Ok(Self {
            callback,
            to,
            value: value.map_or(Ok(U256::ZERO), extract_python_u256)?,
            probes,
            min_net_profit: min_net_profit.map_or(Ok(U256::ZERO), extract_python_u256)?,
            interface,
        })
    }

    /// The contract the composed call targets (EIP-55 checksummed).
    #[getter]
    fn to(&self) -> String {
        self.to.to_checksum(None)
    }

    /// The native value sent with the call, in wei (uint256 integer).
    #[getter]
    fn value<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        u256_to_py(py, &self.value)
    }

    /// The declared pre/post read list, one dict per probe — `{"label", "to",
    /// "data"}` for a contract call (complete calldata) or `{"label",
    /// "account"}` for a native-account balance read.
    #[getter]
    fn probes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let list = PyList::empty(py);
        for spec in &self.probes {
            let entry = PyDict::new(py);
            match spec {
                ProbeSpec::Call { label, to, data } => {
                    entry.set_item("label", label)?;
                    entry.set_item("to", to.to_checksum(None))?;
                    entry.set_item("data", PyBytes::new(py, data))?;
                }
                ProbeSpec::NativeBalance { label, account } => {
                    entry.set_item("label", label)?;
                    entry.set_item("account", account.to_checksum(None))?;
                }
            }
            list.append(entry)?;
        }
        Ok(list.into_any())
    }

    /// The minimum net profit (wei) the default assessment requires to pass
    /// (uint256 integer).
    #[getter]
    fn min_net_profit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        u256_to_py(py, &self.min_net_profit)
    }
}

impl PyPayloadAdapter {
    /// A binding the seam's own tests compose directly (the Python constructor
    /// path is the production door).
    #[cfg(test)]
    #[expect(clippy::expect_used, reason = "the test target is a literal address")]
    pub(crate) fn test_binding(callback: Py<PyAny>, to: &str) -> Self {
        Self {
            callback,
            to: crate::address_utils::parse_address(to)
                .expect("the test target is a valid address"),
            value: U256::ZERO,
            probes: Vec::new(),
            min_net_profit: U256::ZERO,
            interface: None,
        }
    }

    /// Lift this binding into the production adapter handle a core
    /// `degenbot_arbitrage::SimulationDeployment` carries.
    ///
    /// The lift copies the declared data (the bound target, native value, probe
    /// reads, minimum net profit, and the declared interface, when the binding
    /// carries one) and takes a new reference to the held Python callable — no
    /// clone of the callable's behaviour, no second adapter framework: the
    /// lifted value IS this binding, used as the core `ExecutionAdapter`. The
    /// interface travels with it, so the lifted adapter validates the same
    /// encodes at the same single site.
    pub(crate) fn adapter_arc(
        &self,
        py: Python<'_>,
    ) -> std::sync::Arc<dyn ExecutionAdapter + Send + Sync> {
        std::sync::Arc::new(Self {
            callback: self.callback.clone_ref(py),
            to: self.to,
            value: self.value,
            probes: self.probes.clone(),
            min_net_profit: self.min_net_profit,
            interface: self.interface.clone(),
        })
    }
}

impl ExecutionAdapter for PyPayloadAdapter {
    /// The bound transaction, checked against the declared interface when the
    /// binding carried one.
    ///
    /// The pyclass's own `encode` is the declaration's only validation site: a
    /// declaration attached here checks every encode this adapter performs,
    /// including `compose_view` (the trait's default projection calls `encode`)
    /// and the lifted handle a deployment carries.
    ///
    /// # Errors
    ///
    /// `ComposeError::Unsupported` when the callable raises
    /// [`UnsupportedPath`] (the routine capability decline the walk advances
    /// past), any other callable exception as a hard `ComposeError::Encode`, or
    /// `ComposeError::Validation` when the produced transaction violates the
    /// declaration — itself hard, never the routine capability decline.
    fn encode(
        &self,
        path: &PathInfo,
        inputs: &ComposerInputs<'_>,
    ) -> Result<TransactionRequest, ComposeError> {
        let data = compose_with_callable(&self.callback, path, inputs)?;
        let transaction = TransactionRequest::new(self.to, data, self.value);
        if let Some(interface) = &self.interface {
            interface.validate(&transaction)?;
        }
        Ok(transaction)
    }

    fn probe_spec(&self) -> &ProbeSpecs {
        &self.probes
    }

    fn assess_options(&self) -> AssessOptions {
        AssessOptions {
            rule: AssessRule::SumOfDeltas,
            min_net_profit: self.min_net_profit,
        }
    }

    fn fee_policy(&self) -> FeePolicy {
        FeePolicy::default()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Declared interface — the optional transaction declaration (ADR-100 D10)
// ═══════════════════════════════════════════════════════════════════════════

/// The key set an `interface` declaration may carry.
const INTERFACE_KEYS: [&str; 4] = ["abi", "selectors", "target", "value"];

/// The key set a native-value policy dict may carry.
const VALUE_POLICY_KEYS: [&str; 3] = ["exact_wei", "kind", "max_wei"];

/// The key set a validation case may carry.
const CASE_KEYS: [&str; 4] = ["amounts", "expect", "hops", "name"];

/// The key set a case's `amounts` may carry.
const CASE_AMOUNT_KEYS: [&str; 3] = ["consumed_inputs", "hop_outputs", "optimal_input"];

/// The exact key set a case hop of family `v2` or `v3` carries.
const CASE_HOP_POOL_KEYS: [&str; 6] = ["family", "fee", "pool_address", "token0", "token1", "zfo"];

/// The exact key set a case hop of family `v4` carries.
const CASE_HOP_V4_KEYS: [&str; 9] = [
    "currency0",
    "currency1",
    "family",
    "fee",
    "hook_address",
    "pool_id_hex",
    "pool_manager_address",
    "tick_spacing",
    "zfo",
];

/// The exact key set a `decline` expectation carries.
const CASE_EXPECT_DECLINE_KEYS: [&str; 1] = ["kind"];

/// Every key a `transaction` expectation may carry; `selector` is required
/// within it, and `arguments`/`calldata` are mutually exclusive.
const CASE_EXPECT_TRANSACTION_KEYS: [&str; 6] = [
    "arguments",
    "calldata",
    "kind",
    "selector",
    "target",
    "value",
];

/// Render a declaration refusal as the `ValueError` the Python surface raises.
///
/// Reading a declaration is pure data validation (ADR-100 D9), so every refusal
/// is a loud construction error naming the offending field, never a deferred
/// one.
fn declaration_refusal(error: &DeclarationError) -> PyErr {
    PyValueError::new_err(error.to_string())
}

/// The sorted key set of one declaration dict, so an unknown or missing key
/// fails loud instead of silently narrowing the declaration.
fn sorted_keys(dict: &Bound<'_, PyDict>, context: &str) -> PyResult<Vec<String>> {
    let mut keys = Vec::with_capacity(dict.len());
    for key in dict.keys() {
        keys.push(
            key.extract::<String>()
                .map_err(|_| PyValueError::new_err(format!("{context} keys must be str")))?,
        );
    }
    keys.sort();
    Ok(keys)
}

/// Refuse any key `dict` carries that `allowed` does not name.
fn check_keys(dict: &Bound<'_, PyDict>, allowed: &[&str], context: &str) -> PyResult<()> {
    for key in sorted_keys(dict, context)? {
        if !allowed.contains(&key.as_str()) {
            return Err(PyValueError::new_err(format!(
                "{context} declares no `{key}` key (allowed: {allowed:?})"
            )));
        }
    }
    Ok(())
}

/// Require `dict`'s key set to be exactly `expected`, in both directions: a
/// misspelled or missing field is refused, never defaulted.
fn expect_keys(dict: &Bound<'_, PyDict>, expected: &[&str], context: &str) -> PyResult<()> {
    let keys = sorted_keys(dict, context)?;
    if keys.iter().map(String::as_str).eq(expected.iter().copied()) {
        return Ok(());
    }
    Err(PyValueError::new_err(format!(
        "{context} must declare exactly {expected:?} (got {keys:?})"
    )))
}

/// One required field of a declaration dict.
fn required_item<'py>(
    dict: &Bound<'py, PyDict>,
    key: &str,
    context: &str,
) -> PyResult<Bound<'py, PyAny>> {
    dict.get_item(key)?
        .ok_or_else(|| PyValueError::new_err(format!("{context} needs a `{key}`")))
}

/// One optional field of a declaration dict. An explicit Python `None` counts as
/// absent, so a caller may pass a defaulted keyword through unchanged.
fn optional_item<'py>(dict: &Bound<'py, PyDict>, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    match dict.get_item(key)? {
        Some(item) if !item.is_none() => Ok(Some(item)),
        _ => Ok(None),
    }
}

/// Parse one declared address string, naming the field that carried it.
fn declared_address(value: &Bound<'_, PyAny>, field: &str) -> PyResult<Address> {
    let text = value
        .extract::<String>()
        .map_err(|_| PyValueError::new_err(format!("{field} must be an address string")))?;
    crate::address_utils::parse_address(&text)
        .map_err(|error| PyValueError::new_err(format!("{field}: {error}")))
}

/// Decompose a Python `interface=` declaration into the core's compiled
/// [`ContractInterface`].
///
/// The binding — not the caller — serializes the `abi` array to ABI JSON text,
/// parses the addresses, resolves the value policy, and stamps the `target` from
/// the binding's own `to`, because one binding encodes to one address.
/// Construction is pure data validation: it never invokes the compose callable,
/// never reads the chain, and never touches a provider. The declared probe list
/// is checked here too — a probe reading this adapter's own contract must name a
/// declared selector with decodable arguments.
///
/// # Errors
///
/// `ValueError` when the declaration is not a dict, carries an unknown key, has
/// no `abi`, cannot be compiled by the core (malformed ABI JSON, no function,
/// duplicate selector, unknown allow-listed signature, invalid value policy), or
/// declares a target that disagrees with `to`, or a probe to this adapter's own
/// contract that the declaration does not describe.
fn parse_interface(
    declaration: Option<&Bound<'_, PyAny>>,
    to: Address,
    probes: &ProbeSpecs,
) -> PyResult<Option<ContractInterface>> {
    let Some(declaration) = declaration else {
        return Ok(None);
    };
    if declaration.is_none() {
        return Ok(None);
    }
    let dict = declaration.cast::<PyDict>().map_err(|_| {
        PyValueError::new_err("`interface` must be a dict: {'abi', 'selectors', 'target', 'value'}")
    })?;
    check_keys(dict, &INTERFACE_KEYS, "`interface`")?;

    let abi = abi_json_text(&required_item(dict, "abi", "`interface`")?)?;
    let mut interface =
        ContractInterface::from_abi_json(&abi).map_err(|error| declaration_refusal(&error))?;

    if let Some(target) = optional_item(dict, "target")? {
        let target = declared_address(&target, "`interface` `target`")?;
        if target != to {
            return Err(PyValueError::new_err(format!(
                "`interface` declares target {target} but this adapter encodes to {to}: \
                 one binding encodes to one address"
            )));
        }
    }
    interface = interface
        .with_target(to)
        .map_err(|error| declaration_refusal(&error))?;

    if let Some(selectors) = optional_item(dict, "selectors")? {
        let mut signatures = Vec::new();
        let items = selectors.try_iter().map_err(|_| {
            PyValueError::new_err("`interface` `selectors` must be a list of signature strings")
        })?;
        for signature in items {
            signatures.push(signature?.extract::<String>().map_err(|_| {
                PyValueError::new_err("`interface` `selectors` must be a list of signature strings")
            })?);
        }
        interface = interface
            .with_selectors(signatures)
            .map_err(|error| declaration_refusal(&error))?;
    }

    if let Some(policy) = optional_item(dict, "value")? {
        interface = interface.with_value_policy(parse_value_policy(&policy)?);
    }

    interface
        .validate_probes(probes)
        .map_err(|error| declaration_refusal(&error))?;
    Ok(Some(interface))
}

/// The `abi` array as the ABI JSON text the core reads.
///
/// The array is the only part of the declaration that needs a standard
/// serializer, so it goes through the interpreter's own `json` module. A string
/// is taken as the document itself, which is how a caller hands over an emitted
/// artifact verbatim.
fn abi_json_text(abi: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(text) = abi.extract::<String>() {
        return Ok(text);
    }
    abi.py()
        .import("json")?
        .getattr("dumps")?
        .call1((abi,))?
        .extract::<String>()
        .map_err(|_| PyValueError::new_err("`interface` `abi` must be an ABI JSON array"))
}

/// Resolve one native-value policy dict through the core's own reader.
///
/// The forms are `{"kind": "any"}`, `{"kind": "zero"}`, `{"kind": "max",
/// "max_wei": "123"}`, and `{"kind": "exact", "exact_wei": "123"}`. A u256 bound
/// is a decimal string, never a Python float: a bound that did not survive a
/// float would not be a bound.
///
/// # Errors
///
/// `ValueError` for an unknown key, a missing or non-`str` `kind`, a missing
/// `max_wei`/`exact_wei`, or an amount that is not a decimal u256.
fn parse_value_policy(policy: &Bound<'_, PyAny>) -> PyResult<ValuePolicy> {
    let context = "`interface` `value`";
    let dict = policy.cast::<PyDict>().map_err(|_| {
        PyValueError::new_err(
            "`interface` `value` must be a dict: {'kind'} or {'kind', 'max_wei'|'exact_wei'}",
        )
    })?;
    check_keys(dict, &VALUE_POLICY_KEYS, context)?;
    let kind = required_item(dict, "kind", context)?
        .extract::<String>()
        .map_err(|_| PyValueError::new_err(format!("{context} `kind` must be a str")))?;
    let amount_key = match kind.as_str() {
        "max" => Some("max_wei"),
        "exact" => Some("exact_wei"),
        _ => None,
    };
    let amount = match amount_key {
        None => None,
        Some(key) => Some(
            optional_item(dict, key)?
                .ok_or_else(|| {
                    PyValueError::new_err(format!("{context} `{kind}` needs a `{key}`"))
                })?
                .extract::<String>()
                .map_err(|_| {
                    PyValueError::new_err(format!("{context} `{key}` must be a decimal string"))
                })?,
        ),
    };
    ValuePolicy::parse(&kind, amount.as_deref()).map_err(|error| declaration_refusal(&error))
}

/// Turn the Python case list into the core's checked [`ValidationCase`]s.
///
/// Every document is decomposed and validated here, before the first encode, so
/// a malformed case is refused as data instead of being encoded against.
///
/// # Errors
///
/// `ValueError` naming the case and the offending field for any malformed
/// document.
fn validation_cases(cases: &Bound<'_, PyAny>) -> PyResult<Vec<ValidationCase>> {
    let mut parsed = Vec::new();
    let entries = cases
        .try_iter()
        .map_err(|_| PyValueError::new_err("`cases` must be a list of case documents"))?;
    for entry in entries {
        parsed.push(validation_case(&entry?)?);
    }
    Ok(parsed)
}

/// Turn one Python case document into a checked [`ValidationCase`].
///
/// # Errors
///
/// `ValueError` naming the case and the offending field.
fn validation_case(entry: &Bound<'_, PyAny>) -> PyResult<ValidationCase> {
    let dict = entry.cast::<PyDict>().map_err(|_| {
        PyValueError::new_err(
            "each validation case must be a dict: {'name', 'hops', 'amounts', 'expect'}",
        )
    })?;
    check_keys(dict, &CASE_KEYS, "a validation case")?;
    let name = required_item(dict, "name", "a validation case")?
        .extract::<String>()
        .map_err(|_| PyValueError::new_err("a validation case `name` must be a str"))?;
    let context = format!("case `{name}`");
    let hops = case_hops(&required_item(dict, "hops", &context)?, &name)?;
    let amounts = case_amounts(&required_item(dict, "amounts", &context)?, &name)?;
    let expect = case_expect(&required_item(dict, "expect", &context)?, &name)?;
    ValidationCase::new(name, hops, amounts, expect).map_err(|error| declaration_refusal(&error))
}

/// The case's ordered hops, each a real `HopInfo` variant.
///
/// # Errors
///
/// `ValueError` for a hop that is not a dict, an unknown family tag, a missing or
/// unknown per-family field, or an unparseable address.
fn case_hops(hops: &Bound<'_, PyAny>, case: &str) -> PyResult<Vec<HopInfo>> {
    let mut parsed = Vec::new();
    let entries = hops.try_iter().map_err(|_| {
        PyValueError::new_err(format!("case `{case}` `hops` must be a list of hop dicts"))
    })?;
    for (index, hop) in entries.enumerate() {
        parsed.push(case_hop(&hop?, case, index)?);
    }
    Ok(parsed)
}

/// One case hop: the full per-family `HopInfo` shape, never the solver's report
/// descriptor.
///
/// # Errors
///
/// `ValueError` naming the hop index for any field the family's shape does not
/// carry exactly once.
fn case_hop(hop: &Bound<'_, PyAny>, case: &str, index: usize) -> PyResult<HopInfo> {
    let context = format!("case `{case}` `hops[{index}]`");
    let dict = hop
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err(format!("{context} must be a dict")))?;
    let family = required_item(dict, "family", &context)?
        .extract::<String>()
        .map_err(|_| PyValueError::new_err(format!("{context} `family` must be a str")))?;
    let family = case_hop_family(case, &family).map_err(|error| declaration_refusal(&error))?;
    match family {
        HopFamily::V2 | HopFamily::V3 => expect_keys(dict, &CASE_HOP_POOL_KEYS, &context)?,
        HopFamily::V4 => expect_keys(dict, &CASE_HOP_V4_KEYS, &context)?,
    }

    let address = |key: &str| -> PyResult<Address> {
        declared_address(
            &required_item(dict, key, &context)?,
            &format!("{context} `{key}`"),
        )
    };
    let number = |key: &str, width: &str| -> PyResult<Bound<'_, PyAny>> {
        required_item(dict, key, &context).map_err(|_| {
            PyValueError::new_err(format!("{context} `{key}` must be a {width} integer"))
        })
    };
    let zfo = required_item(dict, "zfo", &context)?
        .extract::<bool>()
        .map_err(|_| PyValueError::new_err(format!("{context} `zfo` must be a bool")))?;

    match family {
        HopFamily::V2 => Ok(HopInfo::V2(V2HopInfo {
            pool_address: address("pool_address")?,
            token0_address: address("token0")?,
            token1_address: address("token1")?,
            fee: number("fee", "uint16")?
                .extract::<u16>()
                .map_err(|_| PyValueError::new_err(format!("{context} `fee` must be a uint16")))?,
            zfo,
        })),
        HopFamily::V3 => Ok(HopInfo::V3(V3HopInfo {
            pool_address: address("pool_address")?,
            token0_address: address("token0")?,
            token1_address: address("token1")?,
            fee: number("fee", "uint32")?
                .extract::<u32>()
                .map_err(|_| PyValueError::new_err(format!("{context} `fee` must be a uint32")))?,
            zfo,
        })),
        HopFamily::V4 => Ok(HopInfo::V4(V4HopInfo {
            pool_manager_address: address("pool_manager_address")?,
            pool_id_hex: required_item(dict, "pool_id_hex", &context)?
                .extract::<String>()
                .map_err(|_| {
                    PyValueError::new_err(format!("{context} `pool_id_hex` must be a hex str"))
                })?,
            currency0_address: address("currency0")?,
            currency1_address: address("currency1")?,
            fee: number("fee", "uint32")?
                .extract::<u32>()
                .map_err(|_| PyValueError::new_err(format!("{context} `fee` must be a uint32")))?,
            tick_spacing: number("tick_spacing", "int32")?
                .extract::<i32>()
                .map_err(|_| {
                    PyValueError::new_err(format!("{context} `tick_spacing` must be an int32"))
                })?,
            hook_address: address("hook_address")?,
            zfo,
        })),
    }
}

/// The case's amounts — the solver-driven bundle the encode seam takes.
///
/// # Errors
///
/// `ValueError` for an unknown or missing key, or an amount that is not a
/// non-negative integer (the seam's `int128` bound is checked with the rest of
/// the document).
fn case_amounts(amounts: &Bound<'_, PyAny>, case: &str) -> PyResult<CaseAmounts> {
    let context = format!("case `{case}` `amounts`");
    let dict = amounts
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err(format!("{context} must be a dict")))?;
    check_keys(dict, &CASE_AMOUNT_KEYS, &context)?;
    Ok(CaseAmounts {
        optimal_input: case_amount(
            &required_item(dict, "optimal_input", &context)?,
            case,
            "optimal_input",
        )?,
        hop_outputs: case_amount_list(
            &required_item(dict, "hop_outputs", &context)?,
            case,
            "hop_outputs",
        )?,
        consumed_inputs: case_amount_list(
            &required_item(dict, "consumed_inputs", &context)?,
            case,
            "consumed_inputs",
        )?,
    })
}

/// One case amount, as the seam's unsigned u128 unit.
///
/// # Errors
///
/// `ValueError` naming the case and the field when the value is not a
/// non-negative integer.
fn case_amount(amount: &Bound<'_, PyAny>, case: &str, field: &str) -> PyResult<u128> {
    amount.extract::<u128>().map_err(|_| {
        PyValueError::new_err(format!(
            "case `{case}` amount `{field}` must be a non-negative integer"
        ))
    })
}

/// One case amount list.
///
/// # Errors
///
/// `ValueError` naming the case and the indexed field for any non-integer entry.
fn case_amount_list(amounts: &Bound<'_, PyAny>, case: &str, field: &str) -> PyResult<Vec<u128>> {
    let mut parsed = Vec::new();
    let entries = amounts.try_iter().map_err(|_| {
        PyValueError::new_err(format!("case `{case}` amount `{field}` must be a list"))
    })?;
    for (index, amount) in entries.enumerate() {
        parsed.push(case_amount(&amount?, case, &format!("{field}[{index}]"))?);
    }
    Ok(parsed)
}

/// The case's expectation: the transaction it must produce, or the decline it
/// must report.
///
/// # Errors
///
/// `ValueError` for a missing or unknown `kind`, a `transaction` expectation
/// without a parsable `selector`, both or neither of `arguments`/`calldata`, an
/// argument that does not coerce against the declared parameter types, or a
/// `calldata` golden that is not hex.
fn case_expect(expect: &Bound<'_, PyAny>, case: &str) -> PyResult<Expected> {
    let context = format!("case `{case}` `expect`");
    let dict = expect
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err(format!("{context} must be a dict")))?;
    let kind = required_item(dict, "kind", &context)?
        .extract::<String>()
        .map_err(|_| PyValueError::new_err(format!("{context} `kind` must be a str")))?;
    match kind.as_str() {
        "decline" => {
            expect_keys(dict, &CASE_EXPECT_DECLINE_KEYS, &context)?;
            Ok(Expected::Decline)
        }
        "transaction" => {
            check_keys(dict, &CASE_EXPECT_TRANSACTION_KEYS, &context)?;
            let selector = required_item(dict, "selector", &context)?
                .extract::<String>()
                .map_err(|_| {
                    PyValueError::new_err(format!(
                        "{context} `selector` must be a function signature string"
                    ))
                })?;
            let types = parameter_types(&selector, case)?;
            let call = match (
                optional_item(dict, "arguments")?,
                optional_item(dict, "calldata")?,
            ) {
                (Some(_), Some(_)) => {
                    return Err(PyValueError::new_err(format!(
                        "{context} declares both `arguments` and `calldata`: they are mutually \
                         exclusive"
                    )));
                }
                (Some(arguments), None) => Some(ExpectedCall::Arguments(coerce_arguments(
                    &arguments, &selector, &types, case,
                )?)),
                (None, Some(calldata)) => {
                    Some(ExpectedCall::Calldata(case_calldata(&calldata, case)?))
                }
                (None, None) => None,
            };
            let target = match optional_item(dict, "target")? {
                Some(target) => Some(declared_address(&target, &format!("{context} `target`"))?),
                None => None,
            };
            let value = match optional_item(dict, "value")? {
                Some(value) => Some(extract_python_u256(&value)?),
                None => None,
            };
            Ok(Expected::Transaction(ExpectedTransaction {
                selector,
                target,
                value,
                call,
            }))
        }
        other => Err(PyValueError::new_err(format!(
            "{context} `kind` `{other}` names no expectation (declared: \"transaction\", \
             \"decline\")"
        ))),
    }
}

/// The declared parameter types of one function signature.
///
/// The ABI's own types, resolved through Alloy exactly as the core's declaration
/// resolves them, so a case's expected arguments are coerced against the same
/// types the produced calldata is decoded against — no second ABI
/// implementation, and no dependence on whether the interface declares the
/// signature (an undeclared selector is reported as a case mismatch, which is
/// data, not a malformed document).
///
/// # Errors
///
/// `ValueError` when the signature does not parse or declares an unresolvable
/// parameter type.
fn parameter_types(signature: &str, case: &str) -> PyResult<Vec<DynSolType>> {
    let function = Function::parse(signature).map_err(|error| {
        PyValueError::new_err(format!(
            "case `{case}` `expect.selector` `{signature}` is not a function signature: {error}"
        ))
    })?;
    function
        .inputs
        .iter()
        .map(Specifier::resolve)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            PyValueError::new_err(format!(
                "case `{case}` `expect.selector` `{signature}` declares an unresolvable \
                 parameter type: {error}"
            ))
        })
}

/// Coerce a case's `expect.arguments` list against the signature's parameter
/// types, one value per declared parameter.
///
/// # Errors
///
/// `ValueError` naming the case and the argument index when the list's length
/// disagrees with the signature or a value does not coerce against its declared
/// type.
fn coerce_arguments(
    arguments: &Bound<'_, PyAny>,
    signature: &str,
    types: &[DynSolType],
    case: &str,
) -> PyResult<Vec<DynSolValue>> {
    let refuse = || {
        PyValueError::new_err(format!(
            "case `{case}` `expect.arguments` must be a list of values"
        ))
    };
    let values = arguments
        .try_iter()
        .map_err(|_| refuse())?
        .collect::<PyResult<Vec<_>>>()
        .map_err(|_| refuse())?;
    if values.len() != types.len() {
        return Err(PyValueError::new_err(format!(
            "case `{case}` `expect.arguments` declares {} values for the {} parameters of \
             `{signature}`",
            values.len(),
            types.len()
        )));
    }
    let mut coerced = Vec::with_capacity(types.len());
    for (index, (value, ty)) in values.iter().zip(types).enumerate() {
        coerced.push(python_to_dyn_sol_value(value, ty).map_err(|detail| {
            PyValueError::new_err(format!(
                "case `{case}` `expect.arguments[{index}]`: {detail}"
            ))
        })?);
    }
    Ok(coerced)
}

/// Coerce one Python argument value into the ABI's own dynamic value for `ty`.
///
/// This is the declaration's translation, not a second ABI implementation:
/// every scalar resolves through Alloy's own value parser
/// ([`DynSolType::coerce_str`]), byte strings resolve directly, and arrays and
/// tuples recurse. Encoding, decoding, and selector arithmetic all stay in the
/// core, so a case expectation cannot disagree with the seam about what a
/// declared type means.
fn python_to_dyn_sol_value(
    value: &Bound<'_, PyAny>,
    ty: &DynSolType,
) -> Result<DynSolValue, String> {
    let refuse = |detail: String| format!("`{ty}` {detail}");
    let coerced = match ty {
        DynSolType::Bool => DynSolValue::Bool(
            value
                .extract::<bool>()
                .map_err(|_| refuse("expects a bool".to_string()))?,
        ),
        DynSolType::Uint(_) | DynSolType::Int(_) => {
            let text = value
                .str()
                .map_err(|error| refuse(error.to_string()))?
                .to_str()
                .map_err(|error| refuse(error.to_string()))?
                .to_string();
            ty.coerce_str(&text)
                .map_err(|error| refuse(error.to_string()))?
        }
        DynSolType::Address => match value.extract::<Vec<u8>>() {
            Ok(bytes) if bytes.len() == 20 => DynSolValue::Address(Address::from_slice(&bytes)),
            Ok(bytes) => {
                return Err(refuse(format!(
                    "expects 20 address bytes, got {}",
                    bytes.len()
                )));
            }
            Err(_) => {
                let text = value
                    .extract::<String>()
                    .map_err(|_| refuse("expects an address string".to_string()))?;
                ty.coerce_str(&text)
                    .map_err(|error| refuse(error.to_string()))?
            }
        },
        DynSolType::FixedBytes(size) => match value.extract::<Vec<u8>>() {
            Ok(bytes) if bytes.len() == *size => {
                DynSolValue::FixedBytes(B256::right_padding_from(&bytes), *size)
            }
            Ok(bytes) => {
                return Err(refuse(format!("expects {size} bytes, got {}", bytes.len())));
            }
            Err(_) => {
                let text = value
                    .extract::<String>()
                    .map_err(|_| refuse(format!("expects {size} bytes or a hex string")))?;
                ty.coerce_str(&text)
                    .map_err(|error| refuse(error.to_string()))?
            }
        },
        DynSolType::Bytes => {
            if let Ok(bytes) = value.extract::<Vec<u8>>() {
                DynSolValue::Bytes(bytes)
            } else {
                let text = value
                    .extract::<String>()
                    .map_err(|_| refuse("expects bytes or a hex string".to_string()))?;
                ty.coerce_str(&text)
                    .map_err(|error| refuse(error.to_string()))?
            }
        }
        DynSolType::String => DynSolValue::String(
            value
                .extract::<String>()
                .map_err(|_| refuse("expects a str".to_string()))?,
        ),
        DynSolType::Array(inner) => DynSolValue::Array(coerce_sequence(value, inner, &refuse)?),
        DynSolType::FixedArray(inner, length) => {
            let items = coerce_sequence(value, inner, &refuse)?;
            if items.len() != *length {
                return Err(refuse(format!(
                    "expects {length} elements, got {}",
                    items.len()
                )));
            }
            DynSolValue::FixedArray(items)
        }
        DynSolType::Tuple(types) => {
            let values = sequence(value, &refuse)?;
            if values.len() != types.len() {
                return Err(refuse(format!(
                    "expects {} elements, got {}",
                    types.len(),
                    values.len()
                )));
            }
            let mut items = Vec::with_capacity(values.len());
            for (item, inner) in values.iter().zip(types) {
                items.push(python_to_dyn_sol_value(item, inner)?);
            }
            DynSolValue::Tuple(items)
        }
        other @ DynSolType::Function => {
            return Err(format!("`{other}` is not a coercible argument type"))
        }
    };
    if !coerced.matches(ty) {
        return Err(refuse(format!(
            "does not match the declared type: {coerced:?}"
        )));
    }
    Ok(coerced)
}

/// The Python sequence behind one ABI array or tuple, as one value per element.
fn sequence<'py>(
    value: &Bound<'py, PyAny>,
    refuse: &impl Fn(String) -> String,
) -> Result<Vec<Bound<'py, PyAny>>, String> {
    let items = value
        .try_iter()
        .map_err(|error| refuse(error.to_string()))?;
    items
        .collect::<PyResult<Vec<_>>>()
        .map_err(|error| refuse(error.to_string()))
}

/// Coerce every element of one Python sequence against the same inner type.
fn coerce_sequence(
    value: &Bound<'_, PyAny>,
    inner: &DynSolType,
    refuse: &impl Fn(String) -> String,
) -> Result<Vec<DynSolValue>, String> {
    let values = sequence(value, refuse)?;
    let mut coerced = Vec::with_capacity(values.len());
    for item in &values {
        coerced.push(python_to_dyn_sol_value(item, inner)?);
    }
    Ok(coerced)
}

/// One exact calldata golden, decoded from its hex string.
///
/// # Errors
///
/// `ValueError` when the golden is not a hex string.
fn case_calldata(calldata: &Bound<'_, PyAny>, case: &str) -> PyResult<Bytes> {
    let text = calldata.extract::<String>().map_err(|_| {
        PyValueError::new_err(format!(
            "case `{case}` `expect.calldata` must be a hex string"
        ))
    })?;
    hex::decode(&text).map(Bytes::from).map_err(|error| {
        PyValueError::new_err(format!(
            "case `{case}` `expect.calldata` is not hex: {error}"
        ))
    })
}

/// Render one case failure as the message its report carries.
///
/// The seam's own typed rendering, so a Python report and a Rust `CaseFailure`
/// speak one vocabulary.
fn render_case_failure(failure: &CaseFailure) -> String {
    match failure {
        CaseFailure::Refused(error) => error.to_string(),
        CaseFailure::Mismatch(violation) => violation.to_string(),
    }
}

/// `CaseReport` — one validation case's outcome.
///
/// `name` is the case's report key, `passed` says whether the adapter produced
/// exactly what the case declared (or declined where the case expected a
/// decline), and `violations` carries the rendered failure messages: empty when
/// the case passed, one message when it did not, because a case stops at its
/// first violation. A case run returns one report per case, so every failing
/// case is reported in one pass.
#[pyclass(name = "CaseReport", module = "degenbot._ffi.execution")]
#[derive(Debug)]
pub struct PyCaseReport {
    inner: CaseReport,
}

#[pymethods]
impl PyCaseReport {
    /// The case's name — the report key and the message prefix.
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    /// Whether the case's expectation was met.
    #[getter]
    fn passed(&self) -> bool {
        self.inner.passed()
    }

    /// The rendered failure messages; empty when the case passed.
    #[getter]
    fn violations<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let list = PyList::empty(py);
        if let Err(failure) = &self.inner.result {
            list.append(render_case_failure(failure))?;
        }
        Ok(list.into_any())
    }
}

#[pymethods]
impl PyPayloadAdapter {
    /// Run explicit validation cases against this adapter.
    ///
    /// `cases` is a list of case documents:
    ///
    /// ```text
    /// {
    ///     "name": "two_hop_v2_execute",
    ///     "hops": [{"family": "v2", "pool_address": …, "token0": …, "token1": …,
    ///               "fee": 30, "zfo": True}, …],
    ///     "amounts": {"optimal_input": 10**18, "hop_outputs": […],
    ///                 "consumed_inputs": […]},
    ///     "expect": {"kind": "transaction", "selector": "execute(uint256,…)",
    ///                "arguments": […]},        # or "calldata": "0x…"
    /// }
    /// ```
    ///
    /// `hops` is the full `HopInfo` shape per family (`v2`/`v3`: `pool_address`,
    /// `token0`, `token1`, `fee`, `zfo`; `v4`: `pool_manager_address`,
    /// `pool_id_hex`, `currency0`, `currency1`, `fee`, `tick_spacing`,
    /// `hook_address`, `zfo`), not the solver's report descriptor. Both amount
    /// lists carry exactly one entry per hop. `expect.kind` is `"transaction"`
    /// (then `selector` is required and `arguments` or `calldata` is optional) or
    /// `"decline"` (the adapter must refuse with `ComposeError::Unsupported`, which
    /// a Python callable spells by raising `UnsupportedPath`; any other exception
    /// is a hard encode failure and fails the case whatever it expected).
    ///
    /// The run is explicit, synchronous, and offline: each case becomes a real
    /// `PathInfo` plus `ComposerInputs`, the adapter's own compose callable runs
    /// once per case, and nothing crosses a network boundary — no provider, no
    /// engine, no pool lookup, no path registration, no chain read. The whole
    /// document is decomposed and checked before the first encode, so a malformed
    /// case is refused instead of being encoded against.
    ///
    /// # Errors
    ///
    /// `ValueError` when this adapter declares no `interface` (validation cases
    /// sit above the declared interface, so a silent no-op would be a false
    /// pass), when a case document is malformed (an unknown family tag or field,
    /// an amount list that is not one entry per hop, an unparseable address, both
    /// or neither of `arguments`/`calldata`, an unknown `expect.kind`, an
    /// argument that does not coerce against the declared parameter types), or
    /// when an amount is outside the seam's `int128` bound.
    fn check_cases<'py>(
        &self,
        py: Python<'py>,
        cases: &Bound<'py, PyAny>,
    ) -> PyResult<Vec<Py<PyCaseReport>>> {
        let Some(interface) = &self.interface else {
            return Err(PyValueError::new_err(
                "`check_cases` needs a declared `interface`: validation cases compare against \
                 the declared interface",
            ));
        };
        let cases = validation_cases(cases)?;
        let mut reports = Vec::with_capacity(cases.len());
        for report in run_cases(self, interface, &cases) {
            reports.push(Py::new(py, PyCaseReport { inner: report })?);
        }
        Ok(reports)
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
/// The `degenbot._ffi.execution` Python submodule (declarative
/// `#[pymodule]`), carrying the `ExecutionAdapter` seam lift (ADR-025). The
/// parent module registers the submodule itself and its `sys.modules` entry.
#[pymodule(submodule)]
#[pyo3(module = "degenbot._ffi")]
pub mod execution {
    use pyo3::prelude::*;

    use super::UnsupportedPath;

    #[pymodule_export]
    use super::{
        abi_encode_call, PyCaseReport, PyPayloadAdapter, PyPayloadComposer, PySolveResult,
    };

    /// [`UnsupportedPath`] is a `create_exception!` type (no `_PYO3_DEF`), so it
    /// stays on the submodule's one imperative registration line (the fleet
    /// `PostureRetuneError` pattern) rather than in the declarative export list.
    #[pymodule_init]
    fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add("UnsupportedPath", m.py().get_type::<UnsupportedPath>())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use degenbot_execution::{HopInfo, V2HopInfo};

    const WETH: Address = alloy::primitives::address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    const USDC: Address = alloy::primitives::address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    const EXECUTOR: Address =
        alloy::primitives::address!("7777777777777777777777777777777777777777");

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

    fn sample_amounts() -> ([u128; 2], [u128; 2]) {
        (
            [1_000_000_000_000_000_000u128, 1_000_000_000_000_000_001u128],
            [1_000_000_000_000_000_000u128, 1_000_000_000_000_000_000u128],
        )
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
            let (outputs, consumed) = sample_amounts();
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
    fn a_declined_path_maps_to_unsupported_and_other_errors_stay_hard() {
        Python::attach(|py| {
            // The typed decline keeps the exception's own message and becomes the
            // seam's routine `Unsupported` arm (the walk advances past it).
            let decline = compose_error(
                py,
                &UnsupportedPath::new_err("this contract cannot encode it"),
            );
            assert_eq!(
                decline,
                ComposeError::Unsupported("this contract cannot encode it".to_owned())
            );

            // A bare raise carries the empty message, exactly as `str(exc)` does.
            assert_eq!(
                compose_error(py, &UnsupportedPath::new_err(())),
                ComposeError::Unsupported(String::new())
            );

            // Every other exception is the candidate's own unrecoverable failure.
            for err in [
                PyValueError::new_err("broken encoder"),
                PyTypeError::new_err("wrong shape"),
            ] {
                let classified = compose_error(py, &err);
                assert!(
                    matches!(classified, ComposeError::Encode(_)),
                    "{classified:?}"
                );
            }
        });
    }

    #[test]
    fn view_exposes_descriptors_and_amounts() {
        Python::attach(|py| {
            let callback = py
                .eval(
                    c"lambda result: bytes([len(result.hop_descriptors), \
                     result.hop_descriptors[0]['family'] == 'v2'])",
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
    fn payload_adapter_encodes_a_complete_transaction_with_its_value() {
        Python::attach(|py| {
            let callback = py
                .eval(c"lambda result: bytes([0xAB, 0xCD])", None, None)
                .unwrap();
            let five_ether = U256::from(5_000_000_000_000_000_000u64);
            let value = u256_to_py(py, &five_ether).unwrap();
            let adapter = PyPayloadAdapter::new(
                callback.unbind(),
                "0x7777777777777777777777777777777777777777",
                Some(&value),
                None,
                None,
                None,
            )
            .unwrap();

            let path = sample_path();
            let outputs = [1u128, 2u128];
            let consumed = [1u128, 1u128];
            let inputs = ComposerInputs {
                optimal_input: 1,
                hop_outputs: &outputs,
                consumed_inputs: &consumed,
                opts: degenbot_execution::ComposeOptions,
            };
            let transaction = ExecutionAdapter::encode(&adapter, &path, &inputs)
                .expect("encode through the lift");

            assert_eq!(transaction.to, EXECUTOR);
            assert_eq!(transaction.value, five_ether);
            assert_eq!(transaction.data.to_vec(), vec![0xAB, 0xCD]);
        });
    }

    #[test]
    fn payload_adapter_declares_probes_and_minimum_profit() {
        Python::attach(|py| {
            let callback = py.eval(c"lambda result: b''", None, None).unwrap();
            let probes = py
                .eval(
                    c"[{'label': 'WETH', 'to': '0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2', \
                        'data': bytes([0x70, 0xA0, 0x82, 0x31]) + b'\\x11' * 31 + b'\\x11'}, \
                       {'label': 'ETH', 'account': '0x7777777777777777777777777777777777777777'}]",
                    None,
                    None,
                )
                .unwrap();
            let min_net_profit = u256_to_py(py, &U256::from(1_000_000_000_000_000u64)).unwrap();
            let adapter = PyPayloadAdapter::new(
                callback.unbind(),
                "0x7777777777777777777777777777777777777777",
                None,
                Some(&probes),
                Some(&min_net_profit),
                None,
            )
            .unwrap();

            let declared = adapter.probe_spec();
            assert_eq!(declared.len(), 2);
            let mut expected = vec![0x70, 0xA0, 0x82, 0x31];
            expected.extend_from_slice(&[0x11; 32]);
            assert_eq!(declared[0], ProbeSpec::call("WETH", WETH, expected));
            assert_eq!(declared[1], ProbeSpec::native_balance("ETH", EXECUTOR));
            let options = adapter.assess_options();
            assert_eq!(options.rule, AssessRule::SumOfDeltas);
            assert_eq!(options.min_net_profit, U256::from(1_000_000_000_000_000u64));
            assert_eq!(adapter.fee_policy(), FeePolicy::default());
        });
    }

    #[test]
    fn payload_adapter_rejects_the_retired_probe_triple() {
        Python::attach(|py| {
            let callback = py.eval(c"lambda result: b''", None, None).unwrap();
            let probes = py
                .eval(
                    c"[('WETH', '0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2', bytes([0x70, 0xA0, 0x82, 0x31]))]",
                    None,
                    None,
                )
                .unwrap();
            let err = PyPayloadAdapter::new(
                callback.unbind(),
                "0x7777777777777777777777777777777777777777",
                None,
                Some(&probes),
                None,
                None,
            )
            .unwrap_err();
            assert!(err.to_string().contains("dict"), "{err}");
        });
    }

    #[test]
    fn view_projects_from_canonical_solve_result() {
        // `PySolveResult::from_solve_path` projects the typed view from
        // the canonical `SolvePathResult` (amounts) + `PathInfo` (hop
        // descriptors) — the "one genuinely new surface" (ADR-025 D4).
        use degenbot_pools::solve::SolvePathResult;
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
