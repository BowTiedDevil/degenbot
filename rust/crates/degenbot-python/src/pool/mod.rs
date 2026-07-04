//! `PyO3` seam for the `degenbot-pool-updater` chunk loop (epic `2SFL6I`,
//! task `QZHNZQ`).
//!
//! Thin `#[pyfunction]` wrapper over
//! [`degenbot_pool_updater::run::run_pool_update`] (the Task `CKXCOB` core).
//! Arg extraction → GIL release (`py.detach`) → core call → result wrap.
//! No business logic (three-layer architecture, ADR-005). The "Rust is the
//! engine; Python is the cockpit" framing: Python threads the args + a
//! progress callback + a cancel handle; Rust owns the loop, the RPC fetches,
//! the decode, the DB writes, + the per-chunk transaction.
//!
//! # GIL discipline
//!
//! `py.detach(|| core::run_pool_update(...))` releases the GIL across the
//! WHOLE run (long RPC polls hold NO GIL — `rust/AGENTS.md` §GIL). Only the
//! per-chunk progress callback re-acquires the GIL, briefly, via
//! [`Python::attach`] inside [`PyProgressSink::report_chunk`] (once per
//! chunk, off the async path). A Python-side `KeyboardInterrupt` won't
//! pre-empt mid-chunk (the GIL is released); Task 5's signal handler calls
//! [`CancelHandle::cancel`] (the cooperative flag the loop polls between
//! chunks — see [`degenbot_pool_updater::run::run_pool_update`]'s owned-
//! runtime constraint + §3.3 interrupt contract).
//!
//! # Owned-runtime constraint (D2)
//!
//! [`degenbot_pool_updater::run::run_pool_update`] owns its
//! `tokio::runtime::Runtime`. Calling it from within an existing tokio
//! runtime panics ("Cannot start a runtime from within a runtime"). Task 5's
//! CLI driver runs `run_pool_update` from a worker thread with NO ambient
//! tokio runtime (the standard degenbot CLI doesn't embed one) — the
//! constraint holds. If a future async-Python driver embeds tokio, wrap the
//! call in `tokio::task::spawn_blocking`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use degenbot_pool_updater::run::{self, ChunkProgress, ProgressSink, RunError, UpdateReport};

/// The Python callable signature the seam accepts: a single positional dict
/// argument per chunk (see [`progress_report_to_dict`]). tqdm ticks on the
/// callback. A Python-side exception raised by the callback is logged (via
/// `pyo3-log`) + swallowed — a progress-reporting failure must never break
/// the chunk loop (mirrors `PySubscriberAdapter`'s no-panic contract).
pub(crate) type ProgressCallable = Py<PyAny>;

/// The bridge: a Rust struct holding a `Py<PyAny>` callable, implementing
/// [`ProgressSink`]. Fires once per chunk (the chunk boundary, in
/// [`run_pool_update`]'s sync section after `tx.commit()`/rollback).
/// Re-acquires the GIL via [`Python::attach`] + invokes the callable with a
/// `dict` snapshot of the [`ChunkProgress`] (`chain_id`, `chunk_start`,
/// `chunk_end`, `pools_written`, `liquidity_apply_count`, `committed`).
///
/// Not exposed as a `#[pyclass]` — Python passes a raw `Callable`, + the
/// seam wraps it. This mirrors `PySubscriberAdapter` (a Rust-side adapter
/// over a `Py<PyAny>` callback; the adapter is an impl detail, not a public
/// type). The `Send + Sync` bound the `Arc<dyn ProgressSink>` requires holds:
/// `Py<PyAny>` is `Send` + `Sync` (it's a refcounted handle, not a borrowed
/// `PyAny`).
struct PyProgressSink {
    callback: ProgressCallable,
}

impl PyProgressSink {
    fn new(callback: ProgressCallable) -> Self {
        Self { callback }
    }
}

impl ProgressSink for PyProgressSink {
    fn report_chunk(&self, progress: &ChunkProgress) {
        // Fires from the chunk loop's sync section (GIL released by the
        // `py.detach` around the whole `run_pool_update` call). Re-acquire
        // the GIL briefly; build the dict + invoke. A Python-side exception
        // is logged + swallowed (never breaks the loop).
        Python::attach(|py| {
            let dict = match progress_report_to_dict(py, progress) {
                Ok(d) => d,
                Err(e) => {
                    log::error!("run_pool_update: failed to build progress dict: {e}");
                    return;
                }
            };
            if let Err(err) = self.callback.call1(py, (dict,)) {
                log::warn!("run_pool_update: Python progress callback raised: {err}");
            }
        });
    }
}

/// Build the per-chunk `dict` reported to the Python progress callback.
///
/// Shape: `{"chain_id": int, "chunk_start": int, "chunk_end": int,
/// "pools_written": int, "liquidity_apply_count": int, "committed": bool}`.
/// The committed-chunk field lets tqdm distinguish forward ticks from the
/// rare rolled-back chunk (a progress bar's "skipped" branch).
fn progress_report_to_dict(py: Python<'_>, p: &ChunkProgress) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("chain_id", p.chain_id)?;
    dict.set_item("chunk_start", p.chunk_start)?;
    dict.set_item("chunk_end", p.chunk_end)?;
    dict.set_item("pools_written", p.pools_written)?;
    dict.set_item("liquidity_apply_count", p.liquidity_apply_count)?;
    dict.set_item("committed", p.committed)?;
    Ok(dict.unbind())
}

/// Build the `UpdateReport` return `dict` (matches the `db_heal_database`
/// `dict`-return idiom from `MZ55NP`/`67691f7c`).
///
/// Shape: `{"chain_id": int, "from_block": int, "to_block": int,
/// "chunks_committed": int, "total_pools_written": int,
/// "total_liquidity_applies": int}`.
fn update_report_to_dict(py: Python<'_>, r: &UpdateReport) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("chain_id", r.chain_id)?;
    dict.set_item("from_block", r.from_block)?;
    dict.set_item("to_block", r.to_block)?;
    dict.set_item("chunks_committed", r.chunks_committed)?;
    dict.set_item("total_pools_written", r.total_pools_written)?;
    dict.set_item("total_liquidity_applies", r.total_liquidity_applies)?;
    Ok(dict.unbind())
}

/// `degenbot_rs.run_pool_update(database_path, chain_id, to_block,
/// chunk_size, rpc_url, progress_callback, cancel_handle) -> dict`
///
/// Drive the Rust-owned pool-updater chunk loop for `chain_id`, advancing
/// every active exchange's `last_update_block` to `to_block` (or the chain
/// tip if `to_block is None`). See
/// [`degenbot_pool_updater::run::run_pool_update`] for the §1 three
/// invariants (atomicity / restart-invariance / idempotent re-run) +
/// [`degenbot_pool_updater::run::apply_chunk_writes_on_conn`] for the
/// transaction-semantics core.
///
/// The GIL is released across the WHOLE run (`py.detach`) — only the
/// `progress_callback` re-acquires it briefly, once per chunk (see
/// [`PyProgressSink`]). A Python-side `KeyboardInterrupt` won't pre-empt
/// mid-chunk; Task 5's SIGINT handler calls `cancel_handle.cancel()` (the
/// cooperative flag the loop polls between chunks — §3.3 interrupt contract:
/// SIGINT between chunks → honored immediately; SIGINT mid-chunk → the chunk
/// completes atomically first).
///
/// # Args
///
/// - `database_path` — the writeable `DegenbotDb` path (must already be
///   migrated to the Rust-owned schema; the chunk loop does NOT migrate).
/// - `chain_id` — the chain to advance.
/// - `to_block` — `int` to advance to a specific block; `None` to advance to
///   the chain tip (resolved via `eth_blockNumber`).
/// - `chunk_size` — blocks per chunk (the `MAX_BLOCKS_PER_REQUEST`-aligned
///   batch the RPC fetch + the chunk's single transaction cover).
/// - `rpc_url` — the HTTP RPC endpoint.
/// - `progress_callback` — a Python callable invoked with a per-chunk `dict`
///   `{chain_id, chunk_start, chunk_end, pools_written,
///   liquidity_apply_count, committed}` once per chunk boundary.
/// - `cancel_handle` — a [`CancelHandle`] constructed up front; a
///   `signal.SIGINT` handler calls `.cancel()` on it to stop the run at the
///   next chunk boundary.
///
/// # Returns
///
/// A `dict` `{chain_id, from_block, to_block, chunks_committed,
/// total_pools_written, total_liquidity_applies}`.
///
/// # Raises
///
/// `ValueError` on a DB or RPC failure (the in-flight chunk is rolled back
/// before returning; committed chunks stay durable).
/// `RuntimeError` if cancelled (the `cancel` flag was set; committed chunks
/// durable, in-flight chunk rolled back).
///
/// # Runtime nesting
///
/// Must NOT be called from within an existing `tokio` runtime (the core owns
/// its runtime; nesting panics). The CLI driver runs this from a worker
/// thread with no ambient runtime. See the module docs.
#[pyfunction]
#[pyo3(signature = (
    database_path,
    chain_id,
    to_block,
    chunk_size,
    rpc_url,
    progress_callback,
    cancel_handle,
))]
#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn run_pool_update(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    rpc_url: &str,
    progress_callback: ProgressCallable,
    cancel_handle: &CancelHandle,
) -> PyResult<Py<PyDict>> {
    let path = PathBuf::from(database_path);
    let cancel = cancel_handle.flag.clone();
    let progress: Arc<dyn ProgressSink> = Arc::new(PyProgressSink::new(progress_callback));

    // GIL released across the WHOLE run. `tokio::runtime::Runtime` is built
    // + `block_on`'d inside the core (on this thread, GIL-free); only the
    // progress callback re-enters via `Python::attach`.
    let report = py
        .detach(move || {
            run::run_pool_update(
                &path, chain_id, to_block, chunk_size, rpc_url, cancel, progress,
            )
        })
        .map_err(run_err_to_py)?;

    update_report_to_dict(py, &report)
}

/// `degenbot_rs.CancelHandle` — the cooperative cancel flag for
/// [`run_pool_update`].
///
/// Construct up front; pass to `run_pool_update`; a `signal.SIGINT` handler
/// (installed by the Task 5 CLI driver) calls `.cancel()` to stop the run at
/// the next chunk boundary. The loop polls the flag between chunks (NOT
/// mid-chunk) so a cancel never breaks the chunk's atomicity — the in-flight
/// chunk completes (commit or rollback) before the run returns, the
/// committed chunks stay durable.
///
/// This is preferred over a module-level `set_cancel()` accessor (a
/// `OnceLock<Arc<AtomicBool>>`): the handle is per-run (no global mutable
/// state, no race between concurrent runs, no stale global stuck "set" after
/// a run), + Python owns the lifetime (drop after the run). Mirrors the
/// `Arc<AtomicBool>` the core's `run_pool_update` takes (Task `CKXCOB`).
#[pyclass(name = "CancelHandle")]
pub struct CancelHandle {
    /// The shared flag the chunk loop polls between chunks. `Arc`-cloned into
    /// the core call (the `run_pool_update` signature takes `Arc<AtomicBool>`).
    flag: Arc<AtomicBool>,
}

#[pymethods]
impl CancelHandle {
    /// Construct a fresh cancel handle (flag initially `false`).
    #[new]
    fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation. Honored at the next chunk boundary (after the
    /// current chunk completes atomically). Idempotent.
    fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    /// Whether cancellation was requested. Useful for tests + for a CLI
    /// driver that wants to observe whether its own SIGINT handler fired.
    fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Reset the flag to `false` (reuse the handle for a fresh run).
    /// Not normally needed (construct a fresh handle per run), but offered
    /// for the test harness + the "run, observe, re-run" CLI shell.
    fn reset(&self) {
        self.flag.store(false, Ordering::Release);
    }
}

impl Default for CancelHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Map a [`RunError`] to a Python exception.
///
/// `RunError::Cancelled` becomes `RuntimeError` ("cancelled by cancel flag")
/// so the driver can distinguish a cooperative cancel from a DB/RPC failure
/// (commit-or-rollback integrity holds either way). `Db`/`Provider`/`Runtime`
/// map to `ValueError` (the degenbot convention for operation failures, mir
/// roring [`crate::db::db_err_to_py`] where `Db` variants already land).
fn run_err_to_py(err: RunError) -> PyErr {
    use pyo3::exceptions::{PyRuntimeError, PyValueError};
    match err {
        RunError::Cancelled => PyRuntimeError::new_err("run_pool_update cancelled by cancel flag"),
        other => PyValueError::new_err(other.to_string()),
    }
}

/// Register the pool-updater seam on `m` (feature = "pool").
///
/// # Errors
///
/// Returns a [`PyErr`] if any `add_function`/`add_class` call fails (a name
/// collision); propagated unchanged to the `#[pymodule]` caller.
pub fn add_pool_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(run_pool_update, m)?)?;
    m.add_class::<CancelHandle>()?;
    Ok(())
}

#[cfg(all(test, feature = "auto-initialize"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use degenbot_pool_updater::run::ChunkProgress;

    /// A `CancelHandle` round-trips: fresh → not cancelled; after `cancel()` →
    /// cancelled; after `reset()` → not. (No Python needed — pure Rust state.)
    #[test]
    fn cancel_handle_round_trip() {
        let h = CancelHandle::new();
        assert!(!h.is_cancelled());
        h.cancel();
        assert!(h.is_cancelled());
        h.reset();
        assert!(!h.is_cancelled());
    }

    /// `update_report_to_dict` produces the six-key report dict the `.pyi`
    /// contract promises (the `run_pool_update` return shape).
    #[test]
    fn update_report_dict_shape() {
        Python::attach(|py| {
            let r = UpdateReport {
                chain_id: 1,
                from_block: 10,
                to_block: 99,
                chunks_committed: 3,
                total_pools_written: 7,
                total_liquidity_applies: 4,
            };
            let d = update_report_to_dict(py, &r).unwrap();
            let bound = d.bind(py);
            for k in [
                "chain_id",
                "from_block",
                "to_block",
                "chunks_committed",
                "total_pools_written",
                "total_liquidity_applies",
            ] {
                assert!(bound.contains(k).unwrap(), "missing key {k}");
            }
            assert_eq!(
                bound
                    .get_item("chunks_committed")
                    .unwrap()
                    .unwrap()
                    .extract::<usize>()
                    .unwrap(),
                3
            );
        });
    }

    /// `PyProgressSink` fires the Python callback ONCE per `report_chunk` call,
    /// passing a dict whose 6 keys match the `ChunkProgress` snapshot. This is
    /// the per-chunk GIL re-acquisition path — proving the bridge works without
    /// a live RPC node (the full `run_pool_update` round-trip requires one).
    #[test]
    fn py_progress_sink_callback_fires_once_per_chunk() {
        Python::attach(|py| {
            // Build a Python list + a callback that appends the received dict
            // to it. Bind the list as the lambda's sole positional arg so it
            // captures it (robust vs. closure-cell digging).
            // Build a Python appender-callback: `def cb(d): seen.append(d)`. PyO3's
            // `eval` parses an expression, so construct the callback as an
            // immediately-invoked lambda that closes over the pre-built list.
            let seen = pyo3::types::PyList::empty(py);
            let globals = pyo3::types::PyDict::new(py);
            globals.set_item("seen", seen.as_any()).unwrap();
            // Define `cb(d)` as a real function closing over `seen` (a lambda's
            // global lookup is fragile across the `Python::attach` re-entry; a
            // `def` form pins `seen` in `cb.__globals__`, which pyo3 preserves).
            py.run(c"def cb(d):\n    seen.append(d)", Some(&globals), None)
                .unwrap();
            let cb = globals.get_item("cb").unwrap().unwrap();
            let sink = PyProgressSink::new(cb.unbind());

            // Fire twice — the callback must see BOTH chunks + preserve order.
            sink.report_chunk(&ChunkProgress {
                chain_id: 1,
                chunk_start: 10,
                chunk_end: 19,
                pools_written: 2,
                liquidity_apply_count: 1,
                committed: true,
            });
            sink.report_chunk(&ChunkProgress {
                chain_id: 1,
                chunk_start: 20,
                chunk_end: 29,
                pools_written: 0,
                liquidity_apply_count: 0,
                committed: false,
            });

            // `seen` now holds 2 dicts. Assert the count + a field from each.
            assert_eq!(seen.len(), 2, "callback should have fired once per chunk");
            let first = seen.get_item(0).unwrap();
            assert_eq!(
                first
                    .get_item("chunk_end")
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                19
            );
            assert!(first
                .get_item("committed")
                .unwrap()
                .extract::<bool>()
                .unwrap());
            let second = seen.get_item(1).unwrap();
            assert_eq!(
                second
                    .get_item("chunk_end")
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                29
            );
            assert!(!second
                .get_item("committed")
                .unwrap()
                .extract::<bool>()
                .unwrap());
        });
    }
}
