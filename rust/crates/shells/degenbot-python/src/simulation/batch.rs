//! `BatchExecutor` — the `PyO3` seam over the core batch executor
//! (`degenbot-batch-executor`, ADR-032 naming: the pyclasses carry the core
//! names, no `Py` prefix).
//!
//! The driver (the settlement cockpit) constructs ONE executor per session
//! with resolved policy values — the in-flight cap as a plain count, the
//! thin-margin floor, the relay posture (the `SubmissionTarget` + the
//! broadcast fan-out), the dry-run/inject guards — then:
//!
//! 1. `enqueue`es each streamed solver batch as a
//!    [`BatchWorkRow`] (the runner's `BatchWork` dataclass, extracted by
//!    field name);
//! 2. drains the Batch outcome records via `next_outcome` (the display fold
//!    lives in the driver);
//! 3. polls `raise_if_failed` (the loud-abort rule) and `shutdown`s at
//!    teardown.
//!
//! NO Python closures on the hot path (the task's contract): the two
//! production leaves are the core's own `EngineLeaves`; the only GIL work on
//! this seam is the enqueue-time path resolution (the same point in the flow
//! the pre-cut-over seam resolved, on the driver's loop thread).
//!
//! Path resolution: the core's `PathResolver` is a session-shared map the
//! enqueue path fills (one engine projection call per DISTINCT id per batch
//! — the payload arm's resolve miss is the loud `ValueError` the
//! `merge_payload_results_py` contract pins; a RAW-row miss is left
//! unresolved and the core folds it as the typed `SkipResolveMiss`, decision
//! (a) of task A6SXEH).
//!
//! # GIL discipline (ADR-005 §3 C)
//!
//! Construction + `enqueue` are GIL-held (arg extraction + the resolve). The
//! leaves release the GIL for the whole sim/submit pipeline (the core's
//! future body never touches Python); `next_outcome` releases it across the
//! drain await.
//!
//! The `use_v4_batch` encode axis defaults `false` — the pre-cut-over driver
//! never set it (the `assemble_dispatch_candidates_py` default); a declared
//! config key arrives later as its own value.

use std::collections::HashMap;
use std::sync::Arc;

use crate::bot::engine::PyArbEngine;
use crate::prelude::*;
use crate::rpc::async_provider::PyAsyncAlloyProvider;
use crate::simulation::assembly::RawEngineResult;
use crate::simulation::dispatch::payload_row_from_dict;
use crate::simulation::outcome::{path_info_to_py_dict, sim_failure_to_dict};
use crate::submission::dispatcher::PyDispatcher;
use crate::submission::signer::PyTxSigner;
use crate::submission::submit::{
    settlement_lane, settlement_lane_for, skip_reason_to_py, PyReceiptProbe,
};
use degenbot_arbitrage::BlockPriorityFees;
use degenbot_batch_executor::record::{
    FailureDetail, FailureKind, SimReceipt, SimulateVerdict, SubmitVerdict,
};
use degenbot_batch_executor::row::{PayloadRow, RawResult};
use degenbot_batch_executor::{
    BatchExecutor, BatchOutcome, BatchOutcomeSet, BatchWork, ExecutorConfig, PathResolver,
};
use degenbot_executor::composers::{EncodeOptions, PathInfo};
use degenbot_submission::SubmissionTarget;
use pyo3::exceptions::PyValueError;
use pyo3::types::{PyAny, PyDict, PyList};

/// The loud-abort prefix the executor's leaf-failure surface carries (the
/// same contract the Python-leaf pipeline seam uses).
const LEAF_FAILURE_PREFIX: &str =
    "sim-submit-pipeline leaf task failed - aborting the consumer loudly: ";

/// Map a stored leaf failure to the cockpit's loud-abort `RuntimeError`.
fn leaf_failure_err(failure: degenbot_submission::PipelineFailure) -> PyErr {
    let degenbot_submission::PipelineFailure { detail } = failure;
    pyo3::exceptions::PyRuntimeError::new_err(format!("{LEAF_FAILURE_PREFIX}{detail}"))
}

/// The session-shared path map behind the core's `PathResolver` (see the
/// module docs: enqueue fills it, the core's assembly stage reads it
/// GIL-free).
#[derive(Default)]
struct SharedPathMap(Arc<parking_lot::RwLock<HashMap<u64, PathInfo>>>);

impl PathResolver for SharedPathMap {
    fn resolve(&self, path_id: u64) -> Option<PathInfo> {
        self.0.read().get(&path_id).cloned()
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
/// `record()` — the ONE serializer shape `DispatchOutcome.failures` emits.
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

    /// The failure-bucket label (`classify_revert` output or the
    /// orchestration-only tag).
    #[getter]
    fn bucket(&self) -> &str {
        &self.inner.bucket
    }

    /// The full render record — the `DispatchOutcome.failures` dict shape.
    fn record<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        sim_failure_to_dict(py, &self.inner)
    }
}

/// Stage 1 — the assembly verdict (the core's typed skip taxonomy).
#[pyclass(
    name = "AssemblyVerdict",
    module = "degenbot._ffi.simulation",
    skip_from_py_object
)]
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
#[pyclass(
    name = "SimulateVerdict",
    module = "degenbot._ffi.simulation",
    skip_from_py_object
)]
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
#[pyclass(
    name = "SubmitVerdict",
    module = "degenbot._ffi.simulation",
    skip_from_py_object
)]
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
#[pyclass(
    name = "BatchOutcome",
    module = "degenbot._ffi.simulation",
    skip_from_py_object
)]
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
    /// (`path_type` + per-hop dicts — the ONE serializer shape
    /// `DispatchOutcome.path_infos` emits).
    fn path_info_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        path_info_to_py_dict(py, &PathInfo::new(self.inner.path_info.hops.clone()))
    }
}

/// One drained batch: the candidate outcome records + the RAW submit-lane
/// records the Python drain renders (the `[dispatch]` per-record lines +
/// the silent-veto smoke FSM).
#[pyclass(
    name = "BatchOutcomeSet",
    module = "degenbot._ffi.simulation",
    skip_from_py_object
)]
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

/// The enqueue payload: one streamed solver batch (the runner's `BatchWork`
/// dataclass, extracted by field name — the `payloads` dict's per-path
/// inline-sim payload rows arrive as opaque dicts and are parsed crate-side).
#[derive(FromPyObject)]
struct BatchWorkRow {
    results: Vec<RawEngineResult>,
    payloads: Option<HashMap<u64, Py<PyAny>>>,
    current_block: u64,
    base_fee_next: u128,
    block_timestamp: u64,
}

/// The per-session executor over the core batch choreography.
#[pyclass(
    name = "BatchExecutor",
    module = "degenbot._ffi.simulation",
    skip_from_py_object
)]
pub struct PyBatchExecutor {
    executor: Arc<BatchExecutor>,
    engine: Py<PyArbEngine>,
    paths: SharedPathMap,
    dispatcher: Arc<std::sync::Mutex<degenbot_submission::Dispatcher>>,
}

#[pymethods]
impl PyBatchExecutor {
    /// Enqueue one batch: the rows enter the core's ordered lane (assembly,
    /// sim, and submit run internally; the records surface on
    /// `next_outcome`).
    ///
    /// GIL-held path resolution happens HERE (the pre-cut-over seam's resolve
    /// point, on the driver's loop thread): payload rows resolve LOUD (a miss
    /// is a `ValueError` — the pinned string survives) and raw rows resolve
    /// leniently (a miss stays unresolved; the core folds the typed
    /// `SkipResolveMiss`).
    ///
    /// # Errors
    ///
    /// `ValueError`: a payload dict is malformed, or a payload row's
    /// `path_id` is not registered in the engine.
    fn enqueue(&self, py: Python<'_>, work: BatchWorkRow) -> PyResult<()> {
        let engine = self.engine.borrow(py);
        let mut map = self.paths.0.write();
        let mut payload_rows: Vec<PayloadRow> = Vec::new();
        if let Some(payloads) = &work.payloads {
            for (path_id, value) in payloads {
                let dict = value
                    .bind(py)
                    .cast::<PyDict>()
                    .map_err(|_| PyValueError::new_err("payload entries must be dicts"))?;
                let row = payload_row_from_dict(dict)?;
                if row.path_id != *path_id {
                    return Err(PyValueError::new_err(format!(
                        "payload dict path_id {} disagrees with its key {}",
                        row.path_id, path_id
                    )));
                }
                let info = engine
                    .path_info_for_core(py, row.path_id)
                    .ok_or_else(|| {
                        PyValueError::new_err(format!(
                            "path_id {} is not registered in this engine; \
                             the payload pool keys cannot be derived",
                            row.path_id
                        ))
                    })
                    .and_then(|r| r.map_err(|e| PyValueError::new_err(format!("{e}"))))?;
                map.insert(row.path_id, info);
                payload_rows.push(row);
            }
        }
        let mut rows: Vec<RawResult> = Vec::with_capacity(work.results.len());
        for row in work.results {
            let raw = RawResult {
                path_id: row.path_id,
                optimal_input: row.optimal_input,
                profit: row.engine_profit,
                hop_outputs: row.hop_outputs,
                consumed_inputs: row.consumed_inputs,
                solve_block: row.solve_block,
                state_nonces: row.state_nonces,
            };
            // The raw arm's resolve is LENIENT (decision a): a miss stays
            // unresolved — the core's assembly stage folds the typed
            // `SkipResolveMiss`. A projection BUILD error stays loud (the
            // pre-cut-over seam's `ValueError` fidelity).
            if let Some(resolved) = engine.path_info_for_core(py, raw.path_id) {
                let info = resolved.map_err(|e| PyValueError::new_err(format!("{e}")))?;
                map.insert(raw.path_id, info);
            }
            rows.push(raw);
        }
        drop(map);
        drop(engine);

        // The per-block priority-fee percentiles from the dispatcher's ring
        // (the latest recorded block's p10/p50 — the same lookup
        // `dispatch_profitable_py` performed pre-cut-over).
        let block_priority_fees: Option<BlockPriorityFees> = {
            #[expect(clippy::expect_used)] // invariant-guarded (documented)
            let guard = self.dispatcher.lock().expect("dispatcher mutex poisoned");
            guard
                .block_priority_fees()
                .last_key_value()
                .map(|(&block, fees)| BlockPriorityFees {
                    block,
                    p10: alloy::primitives::U256::from(*fees.get(&10).unwrap_or(&0)),
                    p50: alloy::primitives::U256::from(*fees.get(&50).unwrap_or(&0)),
                })
        };

        let _guard = crate::runtime::get_runtime().enter();
        self.executor.enqueue(BatchWork {
            rows,
            payloads: payload_rows,
            current_block: work.current_block,
            base_fee_next: work.base_fee_next,
            block_timestamp: work.block_timestamp,
            block_priority_fees,
        });
        Ok(())
    }

    /// Await the next batch's outcome set (`None` once the executor is shut
    /// down and drained — the drain loop's terminator).
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

    /// Re-raise the first stored leaf failure in the caller's frame (the
    /// loud-abort rule).
    ///
    /// # Errors
    ///
    /// `RuntimeError` when a sim or submit leaf failed.
    fn raise_if_failed(&self) -> PyResult<()> {
        self.executor.raise_if_failed().map_err(leaf_failure_err)
    }

    /// Drain in-flight work and join the submitter.
    ///
    /// # Errors
    ///
    /// `RuntimeError` when a sim or submit leaf failed.
    fn shutdown<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let executor = Arc::clone(&self.executor);
        crate::ambient_runtime::future_into_py(py, async move {
            executor.shutdown().await.map_err(leaf_failure_err)
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

/// Construct the session executor: the core's value-configured module with
/// every policy value the driver resolved (the cap as a plain count, the
/// thin-margin floor, the relay posture as the `SubmissionTarget` + the
/// broadcast fan-out, the safety guards). Choreography code NEVER crosses.
///
/// Args:
///     `context`: the session `SimulateContext` (provider, addresses, the
///         inject flag, runtime bytecode, warmup slots).
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
///     `sim_concurrency`: the in-flight sim cap (a plain count).
///     `min_profit_margin_bps`: the thin-margin floor (0 disables).
///     `dry_run`: skip live submission.
///     `inject_code_guard`: skip live submission (injected-code sessions).
///     `erc6909_profit`/`use_v4_batch`: the encode options stamped onto every
///         assembled candidate.
///     `max_candidates`: the per-batch sim cap; `0` = no cap.
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
    sim_concurrency,
    min_profit_margin_bps,
    dry_run,
    inject_code_guard,
    erc6909_profit = false,
    use_v4_batch = false,
    max_candidates = 0,
    broadcast_providers = None,
))]
#[expect(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    reason = "the construction boundary carries every injected policy value"
)]
pub fn build_batch_executor_py(
    py: Python<'_>,
    context: &crate::simulation::context::PySimulateContext,
    dispatcher: &PyDispatcher,
    engine: Py<PyArbEngine>,
    signer: &PyTxSigner,
    submit_provider: &PyAsyncAlloyProvider,
    operator_nonce: u64,
    sim_concurrency: usize,
    min_profit_margin_bps: u64,
    dry_run: bool,
    inject_code_guard: bool,
    erc6909_profit: bool,
    use_v4_batch: bool,
    max_candidates: usize,
    broadcast_providers: Option<Vec<PyRef<'_, PyAsyncAlloyProvider>>>,
) -> PyResult<PyBatchExecutor> {
    crate::ambient_runtime::ensure_async_runtime_bound();
    let engine_ref = engine.borrow(py);
    let paths = SharedPathMap::default();
    let config = ExecutorConfig {
        sim_concurrency: sim_concurrency.max(1),
        // `0` = no cap: the pre-cut-over driver never carried a per-batch cap
        // (the fan-out's own `MAX_SIMULATE_CONCURRENT` bound always applied).
        max_candidates: if max_candidates == 0 {
            usize::MAX
        } else {
            max_candidates
        },
        min_profit_margin_bps,
        opts: EncodeOptions {
            erc6909_profit,
            use_v4_batch,
            ..Default::default()
        },
        resolver: Arc::new(SharedPathMap(Arc::clone(&paths.0))),
        // Decision b: the suppression registry is the dispatcher's
        // cross-block feedback arc, DISTINCT from the batch-local
        // payload-served set the enqueue path derives.
        suppression: dispatcher.suppression_arc(),
        divergence: dispatcher.pool_divergence_arc(),
        fot: dispatcher.fot_registry_arc(),
        provider: submit_provider.provider_arc(),
        executor_owner: context.executor_owner,
        executor_address: context.executor_address,
        weth_address: context.weth_address,
        pool_manager_address: context.pool_manager_address,
        multicall3_address: context.multicall3_address,
        inject_code: context.inject_code,
        injected_address: context.injected_address,
        runtime_bytecode: context.runtime_bytecode.clone(),
        warmup: context.warmup,
        bot_state: Some(engine_ref.bot_state_arc()),
        warm_cache: Some(engine_ref.warm_code_cache_arc()),
        dispatcher: dispatcher.inner_arc(),
        signer: Arc::new(signer.signer().clone()),
        probe: Arc::new(PyReceiptProbe::new(&submit_provider.provider_arc())),
        nonce_lane: settlement_lane_for(settlement_lane(), operator_nonce),
        dry_run,
        inject_code_guard,
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
    drop(engine_ref);
    // The core constructor spawns the ordered submit loop, so it must run
    // inside the shared ambient runtime (the Python event-loop thread is not
    // itself in a tokio context).
    let executor = {
        let _guard = crate::runtime::get_runtime().enter();
        Arc::new(BatchExecutor::new(config))
    };
    Ok(PyBatchExecutor {
        executor,
        engine,
        paths,
        dispatcher: dispatcher.inner_arc(),
    })
}
