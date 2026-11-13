//! The typed public execution-probe protocol over the core `ProbeExecutor`:
//! `ExecutionProbeRequest` in, `ProbeReport` out, one call one outcome.
//!
//! # The design problem this seam answers
//!
//! An explicit execution probe asks ONE question: "what do the production
//! assembly, encoder, and EVM2 simulation say about THIS amount vector at THIS
//! block?" The ordered lane answers a different question (session batches in
//! arrival order, published on one shared drain), so serving a probe through it
//! forced every caller to CORRELATE: enqueue a well-formed batch, then find its
//! own record among whatever the lane drained next. Two concurrent callers could
//! take one another's answers, and a cancelled caller left an outcome queued for
//! its successor — which is why the retired `BatchExecutor::probe_path` entry
//! needed a session-side lock and an abandoned-probe reaper.
//!
//! This module retires that shape instead of managing it. The probe value is a
//! [`degenbot_batch_executor::ProbeExecutor`]: ONE call in, THAT call's own
//! outcome out, correlated through a per-call channel, with an abort-on-drop
//! stage task. Non-submission is structural — the probe construction
//! ([`degenbot_batch_executor::SimRuntime`]) cannot name a signer, a nonce lane,
//! a relay fan-out, or a broadcast target — so no request field, no policy flag,
//! and no refusal is needed to keep a probe off the wire.
//!
//! # The protocol values
//!
//! * [`PyExecutionProbeRequest`] — the typed ask: path ID, fixed input, the
//!   caller-supplied hop outputs and consumed inputs, and the block context. It
//!   validates itself at construction, so a malformed vector cannot reach the
//!   stages.
//! * [`PyProbeReport`] — the typed answer: the per-candidate records (the
//!   assembly verdict, the simulate verdict with its receipt or typed failure)
//!   tagged with the capital posture the call ran under, plus the production
//!   encoder's joined rows as planning evidence ([`PyProbeEncodedCall`]:
//!   calldata, native value, target). Nothing in it can be submitted.
//!
//! # GIL discipline (ADR-005)
//!
//! Construction and argument extraction are GIL-held; the probe await releases
//! the GIL (`future_into_py`), and the stage body never re-enters Python.

use std::sync::Arc;

use crate::bot::engine::{EngineStages, PyArbEngine};
use crate::prelude::*;
use crate::rpc::async_provider::PyAsyncAlloyProvider;
use crate::simulation::batch::{EngineResolver, PyBatchOutcome};
use crate::simulation::deployment::{registry_error_to_py, PySimulationDeployment};
use crate::simulation::identity::PyCandidateId;
use crate::submission::dispatcher::PyDispatcher;
use degenbot_batch_executor::{
    BatchWork, ProbeCapital, ProbeConfig, ProbeError, ProbeExecutor, ProbeOutcome, RawResult,
    SimRuntime,
};
use degenbot_submission::SubmitCandidate;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::types::PyBytes;

/// One probe call's typed ask: the path, the caller's fixed input, the amount
/// vector the caller computed for it, and the block it belongs to.
///
/// This is the ONE request shape the public probe surface accepts. It carries
/// no deployment, no signer, no nonce seed, no relay endpoint, and no
/// submission stance — a probe cannot ask to be submitted because the request
/// has no field for it, and neither does the executor it is handed to.
///
/// The vector fields are the caller's, not derived here: the Python boundary
/// keeps its exact-input quote derivation (one amount per hop, quoted at the
/// current pool state), and this value only states what the stages must
/// simulate. `hop_outputs` and `consumed_inputs` are one entry per hop, in
/// path order; `optimal_input` is the probe's fixed input (hop 1's consumed
/// amount).
#[pyclass(
    name = "ExecutionProbeRequest",
    module = "degenbot._ffi.simulation",
    from_py_object
)]
#[derive(Clone)]
pub struct PyExecutionProbeRequest {
    inner: ExecutionProbeRequest,
}

/// The Rust half of [`PyExecutionProbeRequest`]: validated, owned, and free of
/// Python objects, so the `probe` future can carry it across the GIL release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionProbeRequest {
    /// The registered engine path this probe executes.
    pub path_id: u64,
    /// The fixed swap input (wei of the path's first token).
    pub optimal_input: u128,
    /// Per-hop expected outputs, in path order.
    pub hop_outputs: Vec<u128>,
    /// Per-hop consumed inputs, in path order.
    pub consumed_inputs: Vec<u128>,
    /// The block the vector was quoted at (the sim's `block.number`).
    pub current_block: u64,
    /// The next block's base fee (the receipt's gas accounting).
    pub base_fee_next: u128,
    /// The block's timestamp (the sim's `block.timestamp`).
    pub block_timestamp: u64,
}

impl ExecutionProbeRequest {
    /// Project the request onto the ordinary stage input.
    ///
    /// One raw row, no payload rows: a probe's row is caller-supplied, so it is
    /// never payload-served. `state_nonces` are the solve-time nonces the shell
    /// resolves from the engine's path registry (the same resolution the
    /// solver's own rows carry), and `profit` is `0` — a probe's economics come
    /// from the simulation, never from the caller's claim.
    fn to_work(&self, state_nonces: Vec<u64>) -> BatchWork {
        BatchWork {
            rows: vec![RawResult {
                path_id: self.path_id,
                optimal_input: self.optimal_input,
                profit: 0,
                hop_outputs: self.hop_outputs.clone(),
                consumed_inputs: self.consumed_inputs.clone(),
                solve_block: self.current_block,
                state_nonces,
            }],
            payloads: Vec::new(),
            current_block: self.current_block,
            base_fee_next: self.base_fee_next,
            block_timestamp: self.block_timestamp,
            block_priority_fees: None,
        }
    }
}

#[pymethods]
impl PyExecutionProbeRequest {
    /// Build one probe request.
    ///
    /// Args:
    ///     `path_id`: The registered engine path to execute.
    ///     `optimal_input`: The fixed swap input (wei); must be positive.
    ///     `hop_outputs`: One expected output per hop, in path order.
    ///     `consumed_inputs`: One consumed input per hop, in path order.
    ///     `current_block`: The block the vector was quoted at.
    ///     `base_fee_next`: The next block's base fee (wei).
    ///     `block_timestamp`: The quoted block's timestamp.
    ///
    /// Raises:
    ///     `ValueError`: If `optimal_input` is not positive, or the two per-hop
    ///         vectors are empty or of different lengths.
    #[new]
    #[pyo3(signature = (
        path_id,
        optimal_input,
        hop_outputs,
        consumed_inputs,
        current_block,
        base_fee_next,
        block_timestamp,
    ))]
    fn new(
        path_id: u64,
        optimal_input: u128,
        hop_outputs: Vec<u128>,
        consumed_inputs: Vec<u128>,
        current_block: u64,
        base_fee_next: u128,
        block_timestamp: u64,
    ) -> PyResult<Self> {
        if optimal_input == 0 {
            return Err(PyValueError::new_err(
                "ExecutionProbeRequest: optimal_input must be positive",
            ));
        }
        if hop_outputs.is_empty() {
            return Err(PyValueError::new_err(
                "ExecutionProbeRequest: a probe needs at least one hop",
            ));
        }
        if hop_outputs.len() != consumed_inputs.len() {
            return Err(PyValueError::new_err(format!(
                "ExecutionProbeRequest: hop_outputs ({}) and consumed_inputs ({}) must have one \
                 entry per hop",
                hop_outputs.len(),
                consumed_inputs.len()
            )));
        }
        Ok(Self {
            inner: ExecutionProbeRequest {
                path_id,
                optimal_input,
                hop_outputs,
                consumed_inputs,
                current_block,
                base_fee_next,
                block_timestamp,
            },
        })
    }

    /// The registered engine path this probe executes.
    #[getter]
    const fn path_id(&self) -> u64 {
        self.inner.path_id
    }

    /// The fixed swap input (wei).
    #[getter]
    const fn optimal_input(&self) -> u128 {
        self.inner.optimal_input
    }

    /// Per-hop expected outputs, in path order.
    #[getter]
    fn hop_outputs(&self) -> Vec<u128> {
        self.inner.hop_outputs.clone()
    }

    /// Per-hop consumed inputs, in path order.
    #[getter]
    fn consumed_inputs(&self) -> Vec<u128> {
        self.inner.consumed_inputs.clone()
    }

    /// The block the vector was quoted at.
    #[getter]
    const fn current_block(&self) -> u64 {
        self.inner.current_block
    }

    /// The next block's base fee (wei).
    #[getter]
    const fn base_fee_next(&self) -> u128 {
        self.inner.base_fee_next
    }

    /// The quoted block's timestamp.
    #[getter]
    const fn block_timestamp(&self) -> u64 {
        self.inner.block_timestamp
    }
}

/// One production-encoded row the probe's own pass composed: the planning
/// evidence a report keeps ("what would have been sent, at which target, with
/// which value"), never something anything can send.
#[pyclass(name = "ProbeEncodedCall", module = "degenbot._ffi.simulation")]
pub struct PyProbeEncodedCall {
    inner: SubmitCandidate,
}

#[pymethods]
impl PyProbeEncodedCall {
    /// The candidate identity (path + the deployment that composed the row).
    #[getter]
    fn candidate_id(&self) -> PyCandidateId {
        PyCandidateId::wrap(self.inner.candidate_id)
    }

    /// The composed `execute(...)` calldata the production encoder produced.
    #[getter]
    fn execute_calldata<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.execute_calldata)
    }

    /// The encoded transaction's native value (wei).
    #[getter]
    fn value<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        alloy_py::u256_to_py(py, &self.inner.value)
    }

    /// The encoded transaction's `to` — the deployment target, checksummed.
    #[getter]
    fn execution_target(&self) -> String {
        crate::address_utils::address_to_checksum_string(&self.inner.execution_target)
    }

    /// The simulated gross profit (wei).
    #[getter]
    fn gross_profit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        alloy_py::u256_to_py(py, &self.inner.gross_profit)
    }

    /// The simulated net profit (wei) — what a submit floor would read.
    #[getter]
    fn net_profit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        alloy_py::u256_to_py(py, &self.inner.net_profit)
    }

    /// The simulate's raw `gasUsed`.
    #[getter]
    const fn gas_used(&self) -> u64 {
        self.inner.gas_used
    }

    /// The base fee the encoder stamped (wei).
    #[getter]
    const fn base_fee_next(&self) -> u128 {
        self.inner.base_fee_next
    }
}

/// One probe call's typed answer — and only that call can observe it.
///
/// The value is the core [`ProbeOutcome`] plus the capital posture the executor
/// ran under, so a report is self-describing: the records say what the assembly
/// and the simulation decided, the encoded rows say what the production encoder
/// would have sent, and `capital` says which funding posture the simulation
/// assumed. There is no submit-record list and no submit verdict to find — the
/// executor has no submit stage.
#[pyclass(name = "ProbeReport", module = "degenbot._ffi.simulation")]
pub struct PyProbeReport {
    inner: ProbeOutcome,
    capital: ProbeCapital,
}

#[pymethods]
impl PyProbeReport {
    /// The capital posture this call ran under (`"production"`, `"self_fund"`,
    /// or `"zero_balance"`).
    #[getter]
    const fn capital(&self) -> &'static str {
        self.capital.name()
    }

    /// The per-candidate records: the assembly verdict (stage 1) and the
    /// simulate verdict (stage 2, with the receipt or the typed failure).
    ///
    /// Each record's `submit` slot is `None` — there is no submit stage to fill
    /// it — and the slot is kept only so the record vocabulary stays the one the
    /// renderers already read.
    #[getter]
    fn records(&self) -> Vec<PyBatchOutcome> {
        self.inner
            .records
            .iter()
            .map(|record| PyBatchOutcome::wrap(record.clone()))
            .collect()
    }

    /// The production encoder's joined rows for the very candidates above —
    /// the planning evidence (calldata, native value, execution target).
    #[getter]
    fn encoded(&self) -> Vec<PyProbeEncodedCall> {
        self.inner
            .encoded
            .iter()
            .map(|candidate| PyProbeEncodedCall {
                inner: candidate.clone(),
            })
            .collect()
    }
}

/// The public execution-probe executor: the core correlated
/// [`ProbeExecutor`] plus the engine's path registry (the one thing the request
/// cannot carry — a path's solve-time state nonces).
///
/// Construct ONE per capital posture with [`build_probe_executor_py`]. The value
/// holds no queue, no drain, and no session state: concurrent calls are
/// independent, and a dropped call aborts its own stage instead of leaving an
/// outcome for a successor.
#[pyclass(name = "ProbeExecutor", module = "degenbot._ffi.simulation")]
pub struct PyProbeExecutor {
    executor: Arc<ProbeExecutor>,
    stages: Arc<EngineStages>,
    capital: ProbeCapital,
}

#[pymethods]
impl PyProbeExecutor {
    /// The capital posture every call on this executor runs under.
    #[getter]
    const fn capital(&self) -> &'static str {
        self.capital.name()
    }

    /// Run ONE probe call and return ITS OWN report.
    ///
    /// Each call owns its channel, so concurrent calls are independent however
    /// they interleave: no call can observe another's outcome. A dropped call
    /// aborts the stage task it spawned, so nothing is queued for a successor
    /// and no task is left detached — the cancellation needs no session lock and
    /// no reaping.
    ///
    /// Args:
    ///     `request`: The typed [`PyExecutionProbeRequest`].
    ///
    /// Raises:
    ///     `ValueError`: If the request's path is not registered, or its per-hop
    ///         vectors do not match the path's hop count.
    ///     `RuntimeError`: If the stages refuse the row, or the stage task ends
    ///         without producing an outcome.
    fn probe<'py>(
        &self,
        py: Python<'py>,
        request: &PyExecutionProbeRequest,
    ) -> PyResult<Bound<'py, PyAny>> {
        let path_id = request.inner.path_id;
        let state_nonces = self.stages.path_nonces_for(path_id).ok_or_else(|| {
            PyValueError::new_err(format!(
                "ExecutionProbeRequest: path {path_id} is not registered"
            ))
        })?;
        if state_nonces.len() != request.inner.hop_outputs.len() {
            return Err(PyValueError::new_err(format!(
                "ExecutionProbeRequest: path {path_id} has {} hops but the request carries {}",
                state_nonces.len(),
                request.inner.hop_outputs.len()
            )));
        }
        let work = request.inner.to_work(state_nonces);
        let executor = Arc::clone(&self.executor);
        let capital = self.capital;
        crate::ambient_runtime::future_into_py(py, async move {
            match executor.probe(work).await {
                Ok(inner) => Ok(PyProbeReport { inner, capital }),
                Err(ProbeError::Stage(detail)) => Err(PyRuntimeError::new_err(format!(
                    "the execution probe stage pass failed: {detail}"
                ))),
                Err(ProbeError::Abandoned) => Err(PyRuntimeError::new_err(
                    "the execution probe stage task ended without an outcome",
                )),
            }
        })
    }
}

/// Construct a probe executor for ONE capital posture.
///
/// The construction is the sim half of the session executor's and nothing else:
/// no signer, no nonce seed, no submit provider, no relay endpoints, and no
/// run-mode stance cross this boundary, because a probe has no submit lane to
/// arm. The verdict still supplies every policy value (the thin-margin floor,
/// the ERC6909 encode axis, the inject stance) through the core's own
/// conversion, so a probe's answers are the session's answers.
///
/// Args:
///     `context`: The session `SimulateContext` (provider, addresses, the
///         inject stance's runtime facts, runtime bytecode, warmup slots).
///     `dispatcher`: The session `Dispatcher` (suppression, divergence, `FoT`,
///         priority-fee ring).
///     `engine`: The `ArbEngine` (the path resolver projection, the path
///         registry the nonce lookup reads, `BotState`/warm-code arcs).
///     `provider`: The sim provider handle (`AsyncAlloyProvider`).
///     `capital`: The ONE capital posture: `"production"` (default),
///         `"self_fund"`, or `"zero_balance"`.
///     `deployments`: The session's ADDITIONAL execution deployments, offered
///         after the built-in settlement deployment in the order given.
///     `target_selection`: `"first_compatible"` (the default) or
///         `"all_targets"` — which deployments each path is offered to.
///
/// Raises:
/// # Errors
///
/// `ValueError` on an unknown `capital`/`target_selection` spelling, or when the
/// composed registry is refused (an empty registry, two deployments with one
/// identity, or a deployment owned by another account).
///
/// `RuntimeError` when `simulation.delta_dispatch` is ON and the startup BAL
/// capability probe refuses.
#[pyfunction]
#[pyo3(signature = (
    context,
    dispatcher,
    engine,
    provider,
    *,
    capital = None,
    deployments = None,
    target_selection = None,
))]
#[expect(
    clippy::too_many_arguments,
    reason = "the construction boundary carries every injected sim-side handle"
)]
pub fn build_probe_executor_py(
    py: Python<'_>,
    context: &crate::simulation::context::PySimulateContext,
    dispatcher: &PyDispatcher,
    engine: &PyArbEngine,
    provider: &PyAsyncAlloyProvider,
    capital: Option<&str>,
    deployments: Option<Vec<PyRef<'_, PySimulationDeployment>>>,
    target_selection: Option<&str>,
) -> PyResult<PyProbeExecutor> {
    crate::ambient_runtime::ensure_async_runtime_bound();
    let target_selection = match target_selection {
        None => degenbot_arbitrage::TargetSelection::default(),
        Some(name) => degenbot_arbitrage::TargetSelection::from_name(name).ok_or_else(|| {
            PyValueError::new_err(format!(
                "unknown target_selection {name:?}: expected one of {}",
                degenbot_arbitrage::TargetSelection::NAMES.join(", ")
            ))
        })?,
    };
    let capital = match capital {
        None => ProbeCapital::Production,
        Some(name) => ProbeCapital::from_name(name).ok_or_else(|| {
            PyValueError::new_err(format!(
                "unknown capital {name:?}: expected one of {}",
                ProbeCapital::NAMES.join(", ")
            ))
        })?,
    };
    let stages = engine.stages_arc();
    let runtime = SimRuntime {
        // `0` = no cap: the probe runs ONE row, so a per-batch cap has nothing
        // to drop (`SimStage::build` reads `0` as uncapped).
        max_candidates: 0,
        // The v4-batch axis has no declared key yet (a runtime value).
        use_v4_batch: false,
        probe_capital: capital,
        resolver: Arc::new(EngineResolver::new(Arc::clone(&stages))),
        suppression: dispatcher.suppression_arc(),
        divergence: dispatcher.pool_divergence_arc(),
        fot: dispatcher.fot_registry_arc(),
        provider: provider.provider_arc(),
        binding: context.binding,
        additional_deployments: deployments
            .unwrap_or_default()
            .iter()
            .map(|deployment| deployment.deployment_arc())
            .collect(),
        target_selection,
        injected_address: context.injected_address,
        runtime_bytecode: context.runtime_bytecode.clone(),
        bot_state: Some(engine.bot_state_arc()),
        warm_cache: Some(engine.warm_code_cache_arc()),
    };
    let _ = py;
    let config = ProbeConfig::from_verdict(crate::config::installed_bot_config(), runtime)
        .map_err(|error| registry_error_to_py(&error))?;
    let executor =
        crate::runtime::bridged_block_on(ProbeExecutor::new(config)).map_err(|error| {
            PyRuntimeError::new_err(format!("the execution probe could not start: {error}"))
        })?;
    Ok(PyProbeExecutor {
        executor: Arc::new(executor),
        stages,
        capital,
    })
}
