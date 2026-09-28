//! `PySimSubmitPipeline` — the `PyO3` seam over the core bounded-concurrency +
//! ordered-submit pipeline ([`degenbot_submission::SimSubmitPipeline`]).
//!
//! The core owns the semaphore bound, the FIFO submit lane, and the fail-loud
//! contract; this binding drives it with two Python async callables:
//!
//! - `sim(work) -> outcome | None`: the simulate leaf (candidate shaping + the
//!   GIL-free sim + payload merge). `None` means nothing dispatchable.
//! - `submit(work, outcome) -> None`: the render + submit leaf.
//!
//! Awaiting the Python coroutines from the core's tokio tasks runs them on the
//! caller's asyncio loop through the task locals captured at construction
//! ([`pyo3_async_runtimes::into_future_with_locals`]), so the leaf work stays
//! Python-side without a Rust loop blocking on the GIL.
//!
//! The work payload and the sim outcome cross as opaque Python objects: this
//! seam owns the mechanism, not the payload shape.

use std::sync::Arc;

use degenbot_submission::{
    PipelineFailure, SimFuture, SimLeaf, SimSubmitPipeline, SubmitFuture, SubmitLeaf,
};
use pyo3_async_runtimes::{into_future_with_locals, TaskLocals};

use crate::prelude::*;

/// The loud-abort prefix the cockpit's leaf-failure surface carries.
const LEAF_FAILURE_PREFIX: &str =
    "sim-submit-pipeline leaf task failed - aborting the consumer loudly: ";

/// The Python work payload is opaque to the core pipeline.
///
/// `Py<T>` is not `Clone` without pyo3's deprecated `py-clone` feature, so the
/// core's work clone rides an `Arc` around the owned Python object.
type PyWork = Arc<Py<PyAny>>;

/// The Python sim outcome is opaque to the core pipeline.
type PyOutcome = Py<PyAny>;

/// The Python-async simulate leaf.
struct PySimLeaf {
    sim: Py<PyAny>,
    locals: TaskLocals,
}

impl SimLeaf<PyWork, PyOutcome> for PySimLeaf {
    fn simulate<'a>(&'a self, work: &'a PyWork) -> SimFuture<'a, PyOutcome> {
        Box::pin(async move {
            let awaitable = Python::attach(|py| self.sim.call1(py, (work.as_ref().clone_ref(py),)))
                .map_err(|e| e.to_string())?;
            let future = Python::attach(|py| {
                into_future_with_locals(&self.locals, awaitable.into_bound(py))
            })
            .map_err(|e| e.to_string())?;
            let outcome = future.await.map_err(|e| e.to_string())?;
            let is_none = Python::attach(|py| outcome.is_none(py));
            Ok(if is_none { None } else { Some(outcome) })
        })
    }
}

/// The Python-async submit leaf.
struct PySubmitLeaf {
    submit: Py<PyAny>,
    locals: TaskLocals,
}

impl SubmitLeaf<PyWork, PyOutcome> for PySubmitLeaf {
    fn submit<'a>(&'a self, work: &'a PyWork, outcome: &'a PyOutcome) -> SubmitFuture<'a> {
        Box::pin(async move {
            let awaitable = Python::attach(|py| {
                self.submit
                    .call1(py, (work.as_ref().clone_ref(py), outcome.clone_ref(py)))
            })
            .map_err(|e| e.to_string())?;
            let future = Python::attach(|py| {
                into_future_with_locals(&self.locals, awaitable.into_bound(py))
            })
            .map_err(|e| e.to_string())?;
            future.await.map_err(|e| e.to_string())?;
            Ok(())
        })
    }
}

/// Map a stored leaf failure to the cockpit's loud-abort `RuntimeError`.
fn leaf_failure_err(failure: PipelineFailure) -> PyErr {
    let PipelineFailure { detail } = failure;
    pyo3::exceptions::PyRuntimeError::new_err(format!("{LEAF_FAILURE_PREFIX}{detail}"))
}

/// A bounded concurrent sim fan-out with a single ordered submit lane.
///
/// Construct with the two async callables and the in-flight sim cap; `enqueue`
/// returns immediately while the core schedules the bounded sim and registers
/// the batch in the FIFO submit queue. `raise_if_failed` re-raises the first
/// leaf failure in the caller's frame; `shutdown` drains and joins.
#[pyclass(
    name = "SimSubmitPipeline",
    module = "degenbot._ffi.submission",
    skip_from_py_object
)]
pub struct PySimSubmitPipeline {
    inner: Arc<SimSubmitPipeline<PyWork, PyOutcome>>,
}

#[pymethods]
impl PySimSubmitPipeline {
    /// Build the pipeline over the two Python async leaves.
    ///
    /// The task locals (the running asyncio loop + contextvars) are captured
    /// here so the core's tokio tasks can await the Python callables.
    ///
    /// # Errors
    ///
    /// Raises when there is no running asyncio loop to bind the leaves to.
    #[new]
    #[pyo3(signature = (sim, submit, concurrency))]
    fn new(
        py: Python<'_>,
        sim: Py<PyAny>,
        submit: Py<PyAny>,
        concurrency: usize,
    ) -> PyResult<Self> {
        crate::ambient_runtime::ensure_async_runtime_bound();
        let locals = pyo3_async_runtimes::tokio::get_current_locals(py)?;
        let sim_leaf = Arc::new(PySimLeaf {
            sim,
            locals: locals.clone(),
        });
        let submit_leaf = Arc::new(PySubmitLeaf { submit, locals });
        // The core constructor spawns the ordered submit loop, so it must run
        // inside the shared ambient runtime (the Python event-loop thread is
        // not itself in a tokio context).
        let inner = {
            let _guard = runtime::get_runtime().enter();
            SimSubmitPipeline::new(concurrency, sim_leaf, submit_leaf)
        };
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Background one batch's sim (bounded) and register it in the FIFO queue.
    fn enqueue(&self, work: Py<PyAny>) {
        let _guard = runtime::get_runtime().enter();
        self.inner.enqueue(Arc::new(work));
    }

    /// The configured in-flight sim bound.
    #[getter]
    fn concurrency(&self) -> usize {
        self.inner.concurrency()
    }

    /// How many batches have been enqueued.
    #[getter]
    fn enqueued(&self) -> u64 {
        self.inner.enqueued()
    }

    /// How many batches the ordered submitter has drained.
    #[getter]
    fn submitted(&self) -> u64 {
        self.inner.submitted()
    }

    /// Re-raise the first leaf failure in the caller's frame (loud abort).
    ///
    /// # Errors
    ///
    /// Raises `RuntimeError` when a sim or submit leaf failed.
    fn raise_if_failed(&self) -> PyResult<()> {
        self.inner.raise_if_failed().map_err(leaf_failure_err)
    }

    /// Drain in-flight work and join the submitter.
    ///
    /// # Errors
    ///
    /// Raises `RuntimeError` when a sim or submit leaf failed.
    fn shutdown<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = Arc::clone(&self.inner);
        crate::ambient_runtime::future_into_py(py, async move {
            inner.shutdown().await.map_err(leaf_failure_err)
        })
    }
}
