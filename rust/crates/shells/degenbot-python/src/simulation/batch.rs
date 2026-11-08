//! `BatchExecutor` — the `PyO3` seam over the core batch executor
//! (`degenbot-batch-executor`, ADR-032 naming: the pyclasses carry the core
//! names, no `Py` prefix).
//!
//! The driver (the settlement cockpit) constructs ONE executor per session
//! with resolved policy values — the in-flight cap as a plain count, the
//! thin-margin floor, the relay posture (the `SubmissionTarget` + the
//! broadcast fan-out), the dry-run/inject guards — and the session consumer
//! (`crate::simulation::session`) drives it: the consumer derives each batch's
//! work, enqueues it, and owns the loud-abort rule and the teardown. The driver
//! drains the Batch outcome records via `next_outcome` (the display fold lives
//! in the driver). There is no enqueue door on this seam: a caller that could
//! enqueue a batch could own consumption again.
//!
//! NO Python closures on the hot path (the task's contract): the two
//! production leaves are the core's own `EngineLeaves`, and the path resolver
//! is the engine stage surface itself (GIL-free), so no cell of the pipeline
//! re-enters Python.
//!
//! Path resolution: the core's `PathResolver` is the engine's stage projection
//! ([`EngineResolver`]), read GIL-free by the assembly stage — a payload row's
//! resolve miss is the assembly's loud-abort arm (a payload row is engine-born),
//! and a RAW-row miss is the typed `SkipResolveMiss` skip.
//!
//! # GIL discipline (ADR-005)
//!
//! Construction is GIL-held (arg extraction). The leaves release the GIL for
//! the whole sim/submit pipeline (the core's future body never touches Python);
//! `next_outcome` releases it across the drain await.
//!
//! The `use_v4_batch` encode axis defaults `false` — no driver config key
//! declares it yet; a declared key arrives later as its own value.

use std::sync::Arc;

use crate::bot::engine::PyArbEngine;
use crate::prelude::*;
use crate::rpc::async_provider::PyAsyncAlloyProvider;
use crate::simulation::render::{path_info_to_py_dict, sim_failure_to_dict};
use crate::submission::dispatcher::PyDispatcher;
use crate::submission::signer::PyTxSigner;
use crate::submission::submit::{
    settlement_lane, settlement_lane_for, skip_reason_to_py, PyReceiptProbe,
};
use degenbot_batch_executor::record::{
    FailureDetail, FailureKind, SimReceipt, SimulateVerdict, SubmitVerdict,
};
use degenbot_batch_executor::{
    BatchExecutor, BatchOutcome, BatchOutcomeSet, ExecutorConfig, ExecutorRuntime, PathResolver,
};
use degenbot_bot::arb_engine::EngineStages;
use degenbot_execution::PathInfo;
use degenbot_submission::{PathSuppression, SubmissionTarget};
use pyo3::types::{PyDict, PyList};

/// The core's `PathResolver` over the engine's stage surface.
///
/// The projection is the engine registry's own ([`EngineStages::path_info_for`]):
/// there is no Python-side path map to fill, so a resolve is GIL-free and the
/// assembler can read it from any worker. A projection BUILD error reads as a
/// resolve miss here — a raw row folds it as the typed skip, and a payload row
/// (which must resolve) is the assembly's loud-abort arm either way.
struct EngineResolver(Arc<EngineStages>);

impl PathResolver for EngineResolver {
    fn resolve(&self, path_id: u64) -> Option<PathInfo> {
        self.0.path_info_for(path_id).and_then(Result::ok)
    }
}

/// The renderer-read projection of a profitable sim result (the core's
/// `SimReceipt`).
#[pyclass(
    name = "SimReceipt",
    module = "degenbot._ffi.simulation",
    from_py_object
)]
#[derive(Clone)]
pub struct PySimReceipt {
    inner: SimReceipt,
}

#[pymethods]
impl PySimReceipt {
    /// Gross on-chain profit (wei).
    #[getter]
    fn gross_profit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        alloy_py::u256_to_py(py, &self.inner.gross_profit)
    }

    /// Net profit = gross − gas cost (wei).
    #[getter]
    fn net_profit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        alloy_py::u256_to_py(py, &self.inner.net_profit)
    }

    /// The simulate's raw `gasUsed` (UN-inflated; the 1.5× margin applies at
    /// submit time).
    #[getter]
    fn gas_used(&self) -> u64 {
        self.inner.gas_used
    }

    /// The market-aware priority fee.
    #[getter]
    fn priority_fee(&self) -> u128 {
        self.inner.priority_fee
    }
}

/// The per-candidate failure detail (the core's `FailureDetail`). The render
/// dicts the `[sim-fail]`/`[sim-diag]` renderers consume decode through
/// `record()` — the ONE serializer shape a failure record crosses as.
#[pyclass(
    name = "FailureDetail",
    module = "degenbot._ffi.simulation",
    from_py_object
)]
#[derive(Clone)]
pub struct PyFailureDetail {
    inner: FailureDetail,
}

#[pymethods]
impl PyFailureDetail {
    /// The path that failed.
    #[getter]
    fn path_id(&self) -> u64 {
        self.inner.path_id
    }

    /// The failure-bucket label — the Rust
    /// `degenbot::decoders::revert::classify_revert` output or the
    /// orchestration-only tag.
    #[getter]
    fn bucket(&self) -> &str {
        &self.inner.bucket
    }

    /// The full render record — the `[sim-fail]`/`[sim-diag]` dict shape.
    fn record<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        sim_failure_to_dict(py, &self.inner)
    }
}

/// Stage 1 — the assembly verdict (the core's typed skip taxonomy).
#[pyclass(name = "AssemblyVerdict", module = "degenbot._ffi.simulation")]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyAssemblyVerdict {
    Assembled,
    SkipEmptyHops,
    SkipResolveMiss,
    SkipPayloadServed,
    SkipSuppressed,
    SkipThinMargin,
    SkipDivergentPool,
    SkipFeeOnTransfer,
}

impl PyAssemblyVerdict {
    fn wrap(verdict: degenbot_batch_executor::record::AssemblyVerdict) -> Self {
        match verdict {
            degenbot_batch_executor::record::AssemblyVerdict::Assembled => Self::Assembled,
            degenbot_batch_executor::record::AssemblyVerdict::SkipEmptyHops => Self::SkipEmptyHops,
            degenbot_batch_executor::record::AssemblyVerdict::SkipResolveMiss => {
                Self::SkipResolveMiss
            }
            degenbot_batch_executor::record::AssemblyVerdict::SkipPayloadServed => {
                Self::SkipPayloadServed
            }
            degenbot_batch_executor::record::AssemblyVerdict::SkipSuppressed => {
                Self::SkipSuppressed
            }
            degenbot_batch_executor::record::AssemblyVerdict::SkipThinMargin => {
                Self::SkipThinMargin
            }
            degenbot_batch_executor::record::AssemblyVerdict::SkipDivergentPool => {
                Self::SkipDivergentPool
            }
            degenbot_batch_executor::record::AssemblyVerdict::SkipFeeOnTransfer => {
                Self::SkipFeeOnTransfer
            }
        }
    }
}

/// Stage 2 — the per-candidate simulation outcome (the core's typed enum).
// The `Failed` variant carries the far-larger `SimFailure` projection — the
// boxed core shape unboxed into the Python view.
#[expect(clippy::large_enum_variant)]
#[pyclass(name = "SimulateVerdict", module = "degenbot._ffi.simulation")]
#[derive(Clone)]
pub enum PySimulateVerdict {
    /// The sim committed and net profit reached the floor.
    Profitable {
        /// The renderer-read receipt.
        receipt: PySimReceipt,
    },
    /// Onchain-valid but below the net threshold.
    GasUnprofitable {
        /// The same receipt value the core sorts on.
        receipt: PySimReceipt,
    },
    /// A per-candidate failure record (the `[sim-fail]` input).
    Failed {
        /// The failure detail (`record()` decodes the render dict).
        detail: PyFailureDetail,
    },
    /// An unrecoverable per-path exception (counted, not propagated).
    Exception(),
}

#[pymethods]
impl PySimulateVerdict {
    /// The stable machine label — the display fold's discriminator.
    const fn kind(&self) -> &'static str {
        match self {
            Self::Profitable { .. } => "profitable",
            Self::GasUnprofitable { .. } => "gas-unprofitable",
            Self::Failed { .. } => "failed",
            Self::Exception() => "exception",
        }
    }
}

/// The typed submit-failure taxonomy (the core's `FailureKind`).
#[pyclass(
    name = "FailureKind",
    module = "degenbot._ffi.simulation",
    from_py_object
)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyFailureKind {
    Revert,
    NoProfit,
    Int128Overflow,
    EncodeFailed,
    RpcFailed,
    Stale,
    Other,
}

impl PyFailureKind {
    fn wrap(kind: FailureKind) -> Self {
        match kind {
            FailureKind::Revert => Self::Revert,
            FailureKind::NoProfit => Self::NoProfit,
            FailureKind::Int128Overflow => Self::Int128Overflow,
            FailureKind::EncodeFailed => Self::EncodeFailed,
            FailureKind::RpcFailed => Self::RpcFailed,
            FailureKind::Stale => Self::Stale,
            FailureKind::Other => Self::Other,
        }
    }
}

/// Stage 3 — the submit receipt or typed failure (the core's typed enum; no
/// drain reads inside this slot today — the raw submit-lane records the
/// Python renderers consume ride `BatchOutcomeSet.submit_records`).
#[pyclass(name = "SubmitVerdict", module = "degenbot._ffi.simulation")]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PySubmitVerdict {
    /// The tx was broadcast.
    Submitted(),
    /// The candidate reached the submit lane and did not broadcast.
    Failed {
        /// The typed failure taxonomy member.
        kind: PyFailureKind,
    },
}

impl PySubmitVerdict {
    fn wrap(verdict: SubmitVerdict) -> Self {
        match verdict {
            SubmitVerdict::Submitted => Self::Submitted(),
            SubmitVerdict::Failed(kind) => Self::Failed {
                kind: PyFailureKind::wrap(kind),
            },
        }
    }
}

/// The typed result of one candidate's ordered pass through the pipeline
/// (the core's `BatchOutcome`).
#[pyclass(name = "BatchOutcome", module = "degenbot._ffi.simulation")]
#[derive(Clone)]
pub struct PyBatchOutcome {
    inner: BatchOutcome,
}

#[pymethods]
impl PyBatchOutcome {
    /// The candidate's arb path id.
    #[getter]
    fn path_id(&self) -> u64 {
        self.inner.path_id
    }

    /// The block the batch was consumed at (the liveness heartbeat).
    #[getter]
    fn block(&self) -> u64 {
        self.inner.block
    }

    /// The stage-1 assembly verdict.
    #[getter]
    fn assembly(&self) -> PyAssemblyVerdict {
        PyAssemblyVerdict::wrap(self.inner.assembly)
    }

    /// The stage-1 verdict's stable machine label (`"skip-empty-hops"`, …) —
    /// the core record's own spelling, so a reader never re-derives it here.
    #[getter]
    fn assembly_label(&self) -> &'static str {
        self.inner.assembly.label()
    }

    /// The stage-2 simulate verdict (`None` when the row never reached the
    /// sim stage).
    #[getter]
    fn simulate(&self) -> Option<PySimulateVerdict> {
        self.inner.simulate.as_ref().map(|s| match s {
            SimulateVerdict::Profitable(r) => PySimulateVerdict::Profitable {
                receipt: PySimReceipt { inner: r.clone() },
            },
            SimulateVerdict::GasUnprofitable(r) => PySimulateVerdict::GasUnprofitable {
                receipt: PySimReceipt { inner: r.clone() },
            },
            SimulateVerdict::Failed(d) => PySimulateVerdict::Failed {
                detail: PyFailureDetail {
                    inner: (**d).clone(),
                },
            },
            SimulateVerdict::Exception => PySimulateVerdict::Exception(),
        })
    }

    /// The stage-3 submit verdict (`Some` only after a gas-profitable sim
    /// reaches the submit lane).
    #[getter]
    fn submit(&self) -> Option<PySubmitVerdict> {
        self.inner.submit.map(PySubmitVerdict::wrap)
    }

    /// The combined pool-type label (`"V2-V3"`, `"V4-V2"`, … — the
    /// renderers' `path_type`).
    #[getter]
    fn path_type(&self) -> &str {
        &self.inner.path_info.path_type
    }

    /// The `[profit]`/`[sim-fail]` render dict for this record's path
    /// (`path_type` + per-hop dicts — the ONE hop/path serializer shape).
    fn path_info_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        path_info_to_py_dict(py, &PathInfo::new(self.inner.path_info.hops.clone()))
    }
}

/// One drained batch: the candidate outcome records + the RAW submit-lane
/// records the Python drain renders (the `[dispatch]` per-record lines +
/// the silent-veto smoke FSM).
#[pyclass(name = "BatchOutcomeSet", module = "degenbot._ffi.simulation")]
pub struct PyBatchOutcomeSet {
    inner: BatchOutcomeSet,
}

#[pymethods]
impl PyBatchOutcomeSet {
    /// The per-candidate outcome records, in submit order.
    #[getter]
    fn records(&self) -> Vec<PyBatchOutcome> {
        self.inner
            .records
            .iter()
            .map(|r| PyBatchOutcome { inner: r.clone() })
            .collect()
    }

    /// The submit-lane records as the FFI wire dicts (the
    /// `dispatch_and_submit` shape — `typed_submit_record` decodes them).
    #[getter]
    fn submit_records<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for record in &self.inner.submit_records {
            let dict = PyDict::new(py);
            match record {
                degenbot_submission::SubmitRecord::Submitted {
                    path_id,
                    tx_hash,
                    nonce,
                } => {
                    dict.set_item("kind", "submitted")?;
                    dict.set_item("path_id", path_id)?;
                    dict.set_item("tx_hash", format!("{tx_hash:?}"))?;
                    dict.set_item("nonce", nonce)?;
                }
                degenbot_submission::SubmitRecord::Skipped { path_id, reason } => {
                    dict.set_item("kind", "skipped")?;
                    dict.set_item("path_id", path_id)?;
                    let (reason_label, detail) = skip_reason_to_py(reason);
                    dict.set_item("reason", reason_label)?;
                    if let Some(detail) = detail {
                        dict.set_item("detail", detail)?;
                    }
                }
            }
            list.append(dict)?;
        }
        Ok(list)
    }
}

/// The per-session executor over the core batch choreography.
///
/// The session consumer (`crate::simulation::session`) owns this executor's
/// whole input side; this seam carries the drain and the construction handles
/// the consumer needs (the core arc, the session dispatcher, the suppression
/// registry).
#[pyclass(name = "BatchExecutor", module = "degenbot._ffi.simulation")]
pub struct PyBatchExecutor {
    executor: Arc<BatchExecutor>,
    dispatcher: Arc<std::sync::Mutex<degenbot_submission::Dispatcher>>,
    suppression: Arc<std::sync::Mutex<PathSuppression>>,
}

impl PyBatchExecutor {
    /// The core executor arc (the consumer enqueues on it).
    pub(crate) fn executor_arc(&self) -> Arc<BatchExecutor> {
        Arc::clone(&self.executor)
    }

    /// The session's one dispatcher arc (the consumer's block clock drives it).
    pub(crate) fn dispatcher_arc(&self) -> Arc<std::sync::Mutex<degenbot_submission::Dispatcher>> {
        Arc::clone(&self.dispatcher)
    }

    /// The session's cross-block suppression registry (the consumer drops a
    /// removed path's entry there).
    pub(crate) fn suppression_arc(&self) -> Arc<std::sync::Mutex<PathSuppression>> {
        self.suppression.clone()
    }
}

#[pymethods]
impl PyBatchExecutor {
    /// Await the next batch's outcome set (`None` once the executor is shut
    /// down and drained — the drain loop's terminator).
    ///
    /// Cancelling this future loses no message: the Rust-side
    /// `BatchDrain::next` cancel-safety contract pins that the shared
    /// receiver leaves a message queued when the drain future is dropped. The
    /// session consumer quiesces the lane when its loop ends, which closes this
    /// stream: the drain terminates on its own.
    fn next_outcome<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let drain = self.executor.drain_handle();
        crate::ambient_runtime::future_into_py(py, async move {
            let mut drain = drain;
            Ok(drain
                .next()
                .await
                .map(|set| PyBatchOutcomeSet { inner: set }))
        })
    }

    /// How many batches have been enqueued.
    #[getter]
    fn enqueued(&self) -> u64 {
        self.executor.enqueued()
    }

    /// How many batches the ordered submitter has processed.
    #[getter]
    fn submitted(&self) -> u64 {
        self.executor.submitted()
    }
}

/// Construct the session executor: the core's value-configured module over
/// the INSTALLED verdict — the policy values (the cap, the thin-margin
/// floor, the ERC6909 encode axis, the inject guards) convert from the same
/// resolved config the Python verdict projects, with every default and
/// clamp owned by that conversion. The driver injects only the runtime
/// handles the verdict cannot name. Choreography code NEVER crosses.
///
/// Args:
///     `context`: the session `SimulateContext` (provider, addresses, the
///         inject stance's runtime facts, runtime bytecode, warmup slots).
///     `dispatcher`: the session `Dispatcher` (the coordination arcs ride it:
///         suppression, divergence, `FoT`, priority-fee ring).
///     `engine`: the `ArbEngine` (the path resolver projection + the
///         `BotState`/warm-code arcs).
///     `signer`: the operator `TxSigner` (constructed ONCE; the key never
///         leaves Rust).
///     `submit_provider`: the read/broadcast provider handle.
///     `operator_nonce`: the construction-time chain-read nonce seed (the
///         lane's first stamp never re-issues a consumed nonce; the hosted
///         per-head reconcile maintains it afterwards).
///     `dry_run`: skip live submission (the driver's run-mode stance — a CLI
///         fact, not a declared key).
///     `broadcast_providers`: the relay fan-out (the `RelayPosture` value's
///         endpoints); `None`/empty = the public mempool (read provider
///         only).
///
/// # Errors
///
/// `ValueError` on malformed addresses/args.
#[pyfunction]
#[pyo3(signature = (
    context,
    dispatcher,
    engine,
    signer,
    submit_provider,
    operator_nonce,
    *,
    dry_run,
    broadcast_providers = None,
))]
#[expect(
    clippy::too_many_arguments,
    reason = "the construction boundary carries every injected runtime handle"
)]
pub fn build_batch_executor_py(
    py: Python<'_>,
    context: &crate::simulation::context::PySimulateContext,
    dispatcher: &PyDispatcher,
    engine: &PyArbEngine,
    signer: &PyTxSigner,
    submit_provider: &PyAsyncAlloyProvider,
    operator_nonce: u64,
    dry_run: bool,
    broadcast_providers: Option<Vec<PyRef<'_, PyAsyncAlloyProvider>>>,
) -> PyResult<PyBatchExecutor> {
    crate::ambient_runtime::ensure_async_runtime_bound();
    let suppression = dispatcher.suppression_arc();
    let runtime = ExecutorRuntime {
        // `0` = no cap: the cockpit never carried a per-batch cap (the
        // fan-out's own `MAX_SIMULATE_CONCURRENT` bound always applied).
        max_candidates: 0,
        // The v4-batch axis has no declared key yet (a runtime value).
        use_v4_batch: false,
        dry_run,
        // The path resolver is the engine stage surface itself: the projection
        // is GIL-free, so no cell of the pipeline re-enters Python.
        resolver: Arc::new(EngineResolver(engine.stages_arc())),
        // Decision b: the suppression registry is the dispatcher's
        // cross-block feedback arc, DISTINCT from the batch-local
        // payload-served set each batch's payload rows define.
        suppression: Arc::clone(&suppression),
        divergence: dispatcher.pool_divergence_arc(),
        fot: dispatcher.fot_registry_arc(),
        provider: submit_provider.provider_arc(),
        binding: context.binding,
        executor_address: context.executor_address,
        injected_address: context.injected_address,
        runtime_bytecode: context.runtime_bytecode.clone(),
        bot_state: Some(engine.bot_state_arc()),
        warm_cache: Some(engine.warm_code_cache_arc()),
        dispatcher: dispatcher.inner_arc(),
        signer: Arc::new(signer.signer().clone()),
        probe: Arc::new(PyReceiptProbe::new(&submit_provider.provider_arc())),
        nonce_lane: settlement_lane_for(settlement_lane(), operator_nonce)
            .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?,
        extra_broadcast: broadcast_providers
            .unwrap_or_default()
            .iter()
            .map(|p| p.provider_arc())
            .collect(),
        // The settlement cockpit's posture: the public-mempool fan-out (the
        // relay endpoints ride `extra_broadcast`; a bundle channel is the
        // pending-tx slot, not this strategy's).
        target: SubmissionTarget::Public,
    };
    let _ = py;
    // The policy values convert from the INSTALLED verdict — the same load
    // `resolved_config().values` projects — so the executor cannot disagree
    // with the verdict Python reads.
    let config = ExecutorConfig::from_verdict(crate::config::installed_bot_config(), runtime);
    // The core constructor spawns the ordered submit loop, so it must run
    // inside the shared ambient runtime (the Python event-loop thread is not
    // itself in a tokio context).
    let executor = {
        let _guard = crate::runtime::get_runtime().enter();
        Arc::new(BatchExecutor::new(config))
    };
    Ok(PyBatchExecutor {
        executor,
        dispatcher: dispatcher.inner_arc(),
        suppression,
    })
}

/// The verdict-named executor policy, as the resolution oracle reads it: the
/// SAME conversion the executor construction runs, exposed so a Python test
/// can pin every knob against the verdict's own values.
///
/// Args:
///     `values`: the typed projection (`resolved_config().values`, or a
///         hypothetical resolution's) the policy converts from.
///
/// Returns:
///     `ExecutorPolicyValues`: the converted policy (the cap with its floor,
///     the thin-margin floor, the ERC6909 axis, both inject guards).
#[pyfunction]
#[must_use]
pub fn executor_policy_py(values: &crate::config::ConfigValues) -> ExecutorPolicyValues {
    ExecutorPolicyValues {
        inner: degenbot_batch_executor::ExecutorPolicy::from(values.bot_config()),
    }
}

/// The converted executor policy (see [`executor_policy_py`]). A frozen
/// read-only view: the conversion owns every default and clamp, so there is
/// nothing for a caller to set.
#[pyclass(frozen, module = "degenbot._ffi")]
pub struct ExecutorPolicyValues {
    inner: degenbot_batch_executor::ExecutorPolicy,
}

#[pymethods]
impl ExecutorPolicyValues {
    /// The in-flight sim bound: `simulation.pipeline_concurrency`, floored
    /// at 1 by the conversion.
    #[getter]
    fn sim_concurrency(&self) -> usize {
        self.inner.sim_concurrency
    }

    /// The thin-margin floor: `dispatch.min_profit_margin_bps`.
    #[getter]
    fn min_profit_margin_bps(&self) -> u64 {
        self.inner.min_profit_margin_bps
    }

    /// The ERC6909 encode axis: `dispatch.erc6909_profit`.
    #[getter]
    fn erc6909_profit(&self) -> bool {
        self.inner.erc6909_profit
    }

    /// The sim-injection stance: `simulation.inject_executor_code`.
    #[getter]
    fn inject_code(&self) -> bool {
        self.inner.inject_code
    }

    /// The submit-side guard: the SAME inject stance, converted once.
    #[getter]
    fn inject_code_guard(&self) -> bool {
        self.inner.inject_code_guard
    }
}
