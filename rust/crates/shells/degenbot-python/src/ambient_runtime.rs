//! Python-driver seam for the shared degenbot-core ambient runtime.
//!
//! The verify seams (`aave_updater::verify_touched_positions_on_chain`,
//! `pool::verify_v3/v4_liquidity_map`) removed their per-call multi-thread
//! runtime build (the dead tokio-rt-worker churn source) and now require the
//! CALLER's ambient runtime — a missing one returns the typed VJGZJ2 error
//! ("run under the shared degenbot-core ambient runtime"). Rust consumers
//! enter the runtime naturally (they run on it); a Python driver shell has
//! no way to do that, so this module exposes the minimal primitive: call a
//! Python callable with the shared runtime entered on the calling thread.
//! The orchestration engine stays Rust-owned (AGENTS.md) — this is the thin
//! driver affordance the typed error's message invites.

use pyo3::prelude::*;

/// Bind `pyo3-async-runtimes` to the shared degenbot-core ambient runtime,
/// exactly once per process, before the first `future_into_py` runs.
///
/// GOQWCL (incident 2026-08-21): using pyo3-async's default runtime when the
/// binding never happened is what spawned a SECOND nproc-worker runtime
/// mid-run (24 surprise worker threads at the wedge timestamp). The singleton
/// here is the ONE shared runtime (`degenbot_core::runtime`), and the
/// `OnceLock` makes creation deterministic at first async use; the driver's
/// explicit `driver_boot()` moves that first use back to process start.
pub fn ensure_async_runtime_bound() {
    static BOUND: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    BOUND.get_or_init(|| {
        if pyo3_async_runtimes::tokio::init_with_runtime(degenbot_core::runtime::get_runtime())
            .is_err()
        {
            degenbot_core::diag!(
                domain = pump,
                "pyo3_async_runtimes already bound to a runtime"
            );
        }
    });
}

/// The ONE async seam: every `future_into_py` call site routes through here
/// so a process that never called `driver_boot()` still binds the shared
/// runtime instead of pyo3-async's default. Bounds mirror
/// `pyo3_async_runtimes::tokio::future_into_py` verbatim.
///
/// # Errors
///
/// Propagates the upstream result unchanged; the guard itself cannot fail.
pub fn future_into_py<F, T>(py: Python<'_>, fut: F) -> PyResult<Bound<'_, PyAny>>
where
    F: std::future::Future<Output = PyResult<T>> + Send + 'static,
    T: for<'any> pyo3::IntoPyObject<'any> + Send + 'static,
{
    ensure_async_runtime_bound();
    pyo3_async_runtimes::tokio::future_into_py(py, fut)
}

/// Call `fn_work()` with the shared degenbot-core runtime entered on the
/// calling thread.
///
/// Python drivers + tests that call an ambient-runtime-only verify seam
/// (the VJGZJ2 policy) wrap the call in this helper:
///
/// ```python
/// divergences = call_on_ambient_runtime(
///     partial(verify_touched_positions_on_chain, database_path=..., ...)
/// )
/// ```
///
/// The shared runtime singleton is created on first use (one runtime per
/// process, created deterministically); the enter guard is held for the
/// duration of the call, then the thread-local Handle is restored.
/// Re-entrant — a caller already inside a runtime just re-enters.
///
/// # Args
///
/// - `fn_work` — a zero-argument Python callable. Its return value (or
///   exception) is passed through unchanged.
///
/// # Errors
///
/// Propagates whatever error `fn_work()` raises, unchanged; `add_function`'s
/// registration of this symbol surfaces any binding failure at module init.
///
/// # Returns
///
/// Whatever `fn_work()` returns.
#[pyfunction]
pub fn call_on_ambient_runtime(
    #[expect(unused_variables)] py: Python<'_>,
    fn_work: &Bound<'_, PyAny>,
) -> PyResult<Py<PyAny>> {
    let _guard = degenbot_core::runtime::get_runtime().enter();
    fn_work.call0().map(Bound::unbind)
}

/// Async sibling of [`call_on_ambient_runtime`]: run a **blocking**
/// zero-argument Python callable on the shared runtime's blocking pool and
/// return the awaitable that resolves with its result.
///
/// A synchronous prep/read invoked from inside a coroutine still blocks the
/// event loop even when its heavy Rust half releases the GIL — `detach` only
/// lets *other OS threads* run, while the asyncio loop lives on the awaiting
/// thread. This helper lifts such a call off the loop: the returned awaitable
/// schedules the callable on a `tokio::task::spawn_blocking` thread (entering
/// the shared runtime and acquiring the GIL there), so the loop keeps pumping
/// other coroutines until the prep finishes and the result (or its exception)
/// is ferried back.
///
/// This is the minimal seam for a prep step whose logic must stay
/// Python-side (e.g. an `SQLAlchemy` token resolution + a bulk graph read
/// pending the ADR-052 DB cutover): it moves the *scheduling*, not the logic.
///
/// ```python
/// prepared = await call_blocking_on_ambient_runtime(partial(prepare, ...))
/// ```
///
/// # Args
///
/// - `fn_work` — a zero-argument Python callable. It runs on a blocking
///   thread; its return value (or exception) is delivered to the awaiting
///   coroutine unchanged.
///
/// # Errors
///
/// Propagates whatever error `fn_work()` raises, unchanged. A panicking
/// blocking task surfaces as a `RuntimeError`.
#[pyfunction]
pub fn call_blocking_on_ambient_runtime(
    py: Python<'_>,
    fn_work: Py<PyAny>,
) -> PyResult<Bound<'_, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        tokio::task::spawn_blocking(move || {
            Python::attach(|py| {
                let _guard = degenbot_core::runtime::get_runtime().enter();
                fn_work.call0(py)
            })
        })
        .await
        .map_err(|join_err| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "blocking call failed to join: {join_err}"
            ))
        })?
    })
}
