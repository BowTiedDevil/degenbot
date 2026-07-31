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
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};

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
/// `chunk_end`, `pools_written`, `liquidity_apply_count`, `committed`, `is_final`).
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
                    tracing::error!(%e, "run_pool_update: failed to build progress dict");
                    return;
                }
            };
            if let Err(err) = self.callback.call1(py, (dict,)) {
                tracing::warn!(%err, "run_pool_update: Python progress callback raised");
            }
        });
    }
}

/// Build the per-chunk `dict` reported to the Python progress callback.
///
/// Shape: `{"chain_id": int, "chunk_start": int, "chunk_end": int,
/// "pools_written": int, "liquidity_apply_count": int, "committed": bool,
/// "is_final": bool}`.
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
    dict.set_item("is_final", p.is_final)?;
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
///   liquidity_apply_count, committed, is_final}` once per chunk boundary.
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
    verify_chunk = false,
    *,
    verify_all_interval = None,
    verify_all_at_completion = false,
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
    verify_chunk: bool,
    verify_all_interval: Option<u64>,
    verify_all_at_completion: bool,
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
                &path,
                chain_id,
                to_block,
                chunk_size,
                rpc_url,
                cancel,
                progress,
                verify_chunk,
                verify_all_interval,
                verify_all_at_completion,
            )
        })
        .map_err(run_err_to_py)?;

    update_report_to_dict(py, &report)
}

/// `degenbot_rs.verify_v3_liquidity_map(database_path, rpc_url, chain_id,
/// pool_address, block_number) -> list[dict]`
///
/// Standalone on-chain-truth verification of a V3 pool's COMMITTED liquidity
/// map (read-only — no SQL writes, no event apply). Opens the DB, fetches the
/// pool's `liquidity_positions` + `initialization_maps` rows, builds the
/// in-memory map, + compares EVERY tick + bitmap word against on-chain
/// `IUniswapV3Pool.ticks(int24)` / `tickBitmap(int16)` at `block_number`, all
/// batched through Multicall3. Returns the divergence list (empty = GREEN —
/// the DB matches the chain at `block_number`).
///
/// This is the ad-hoc / spot-check sibling of the pre-commit gate
/// (`run_pool_update(verify=True)`); the gate runs the SAME core verify
/// BEFORE the write commits, while this reads the already-committed state.
/// Per-position `eth_call`s are batched via Multicall3.
///
/// The GIL is released across the whole call (`py.detach`); the runtime +
/// `AlloyProvider` are owned internally (mirrors the aave verify seam).
#[pyfunction]
#[pyo3(signature = (database_path, rpc_url, chain_id, pool_address, block_number))]
#[allow(clippy::needless_pass_by_value)]
fn verify_v3_liquidity_map(
    py: Python<'_>,
    database_path: &str,
    rpc_url: &str,
    chain_id: i64,
    pool_address: &str,
    block_number: u64,
) -> PyResult<Vec<Py<PyDict>>> {
    use degenbot_db::{ComputedLiquidityUpdate, DegenbotDb};
    use degenbot_pool_updater::verify_v3_liquidity_map_on_chain;
    use degenbot_rpc::provider::AlloyProvider;

    let path = PathBuf::from(database_path);
    let addr: alloy::primitives::Address = pool_address
        .parse()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("bad pool_address: {e}")))?;
    let addr_str = addr.to_checksum(None);

    let divergences = py
        .detach(move || {
            use tokio::runtime::Handle;
            let db = DegenbotDb::open(&path).map_err(RunError::Db)?.0;
            let conn = db.lock();
            let state = DegenbotDb::fetch_v3_pool_update_state_on_conn(&conn, chain_id, &addr_str)
                .map_err(RunError::Db)?
                .ok_or_else(|| {
                    RunError::Db(degenbot_db::DbError::Decode(format!(
                        "v3 pool {addr_str} not found on chain {chain_id}"
                    )))
                })?;
            let (tick_bitmap, tick_data) =
                DegenbotDb::fetch_v3_liquidity_map_on_conn(&conn, state.pool_id)
                    .map_err(RunError::Db)?;
            let computed = ComputedLiquidityUpdate {
                pool_id: state.pool_id,
                tick_spacing: state.tick_spacing,
                tick_data,
                tick_bitmap,
                last_event: None,
            };
            let res: Result<Vec<_>, RunError> = if let Ok(handle) = Handle::try_current() {
                let provider = handle.block_on(AlloyProvider::new(rpc_url, 5))?;
                let fut =
                    verify_v3_liquidity_map_on_chain(&provider, addr, &computed, block_number);
                Ok(tokio::task::block_in_place(|| handle.block_on(fut))?)
            } else {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()?;
                let provider = rt.block_on(AlloyProvider::new(rpc_url, 5))?;
                Ok(rt.block_on(verify_v3_liquidity_map_on_chain(
                    &provider,
                    addr,
                    &computed,
                    block_number,
                ))?)
            };
            res
        })
        .map_err(run_err_to_py)?;

    Ok(divergences_to_dicts(py, &divergences))
}

/// `degenbot_rs.verify_v4_liquidity_map(database_path, rpc_url, chain_id,
/// pool_hash, pool_manager_address, block_number) -> list[dict]`
///
/// Standalone on-chain-truth verification of a V4 pool's COMMITTED liquidity
/// map via the singleton `PoolManager.extsload(bytes32[])` at `block_number`
/// (read-only — mirrors [`verify_v3_liquidity_map`]). `pool_hash` is the V4
/// `PoolId` (bytes32 hex, `0x…`); `pool_manager_address` is the deployed V4
/// `PoolManager` singleton (the chain has one — it's the V4 exchange's
/// `factory`). Returns the divergence list (empty = GREEN).
///
/// The GIL is released across the whole call; the runtime + provider are
/// owned internally.
#[pyfunction]
#[pyo3(signature = (database_path, rpc_url, chain_id, pool_hash, pool_manager_address, block_number))]
#[allow(clippy::needless_pass_by_value)]
fn verify_v4_liquidity_map(
    py: Python<'_>,
    database_path: &str,
    rpc_url: &str,
    chain_id: i64,
    pool_hash: &str,
    pool_manager_address: &str,
    block_number: u64,
) -> PyResult<Vec<Py<PyDict>>> {
    use degenbot_db::{ComputedLiquidityUpdate, DegenbotDb};
    use degenbot_pool_updater::verify_v4_liquidity_map_on_chain;
    use degenbot_rpc::provider::AlloyProvider;
    use std::str::FromStr;

    let path = PathBuf::from(database_path);
    let pool_manager: alloy::primitives::Address = pool_manager_address.parse().map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("bad pool_manager_address: {e}"))
    })?;
    let pool_id =
        alloy::primitives::B256::from_str(pool_hash.strip_prefix("0x").unwrap_or(pool_hash))
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("bad pool_hash: {e}")))?;

    let divergences = py
        .detach(move || {
            use tokio::runtime::Handle;
            let db = DegenbotDb::open(&path).map_err(RunError::Db)?.0;
            let conn = db.lock();
            let state = DegenbotDb::fetch_v4_pool_update_state_on_conn(&conn, pool_hash, chain_id)
                .map_err(RunError::Db)?
                .ok_or_else(|| {
                    RunError::Db(degenbot_db::DbError::Decode(format!(
                        "v4 pool {pool_hash} not found on chain {chain_id}"
                    )))
                })?;
            let (tick_bitmap, tick_data) =
                DegenbotDb::fetch_v4_liquidity_map_on_conn(&conn, state.pool_id)
                    .map_err(RunError::Db)?;
            let computed = ComputedLiquidityUpdate {
                pool_id: state.pool_id,
                tick_spacing: state.tick_spacing,
                tick_data,
                tick_bitmap,
                last_event: None,
            };
            let res: Result<Vec<_>, RunError> = if let Ok(handle) = Handle::try_current() {
                let provider = handle.block_on(AlloyProvider::new(rpc_url, 5))?;
                let fut = verify_v4_liquidity_map_on_chain(
                    &provider,
                    pool_manager,
                    pool_id,
                    &computed,
                    block_number,
                );
                Ok(tokio::task::block_in_place(|| handle.block_on(fut))?)
            } else {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()?;
                let provider = rt.block_on(AlloyProvider::new(rpc_url, 5))?;
                Ok(rt.block_on(verify_v4_liquidity_map_on_chain(
                    &provider,
                    pool_manager,
                    pool_id,
                    &computed,
                    block_number,
                ))?)
            };
            res
        })
        .map_err(run_err_to_py)?;

    Ok(divergences_to_dicts(py, &divergences))
}

/// Encode a [`degenbot_pool_updater::LiquidityDivergence`] list as a list of
/// dicts (empty = GREEN). Each dict carries `variant` + the named fields for
/// bisect-able triage (the same shape the pre-commit gate surfaces).
fn divergences_to_dicts(
    py: Python<'_>,
    divergences: &[degenbot_pool_updater::LiquidityDivergence],
) -> Vec<Py<PyDict>> {
    use degenbot_pool_updater::LiquidityDivergence;
    let mut out = Vec::with_capacity(divergences.len());
    for d in divergences {
        let dict = PyDict::new(py);
        match d {
            LiquidityDivergence::TickGross {
                tick,
                expected,
                actual,
            } => {
                dict.set_item("variant", "TickGross").unwrap();
                dict.set_item("tick", tick).unwrap();
                dict.set_item("expected", format!("{expected}")).unwrap();
                dict.set_item("actual", format!("{actual}")).unwrap();
            }
            LiquidityDivergence::TickNet {
                tick,
                expected,
                actual,
            } => {
                dict.set_item("variant", "TickNet").unwrap();
                dict.set_item("tick", tick).unwrap();
                dict.set_item("expected", format!("{expected}")).unwrap();
                dict.set_item("actual", format!("{actual}")).unwrap();
            }
            LiquidityDivergence::BitmapWord {
                word,
                expected,
                actual,
            } => {
                dict.set_item("variant", "BitmapWord").unwrap();
                dict.set_item("word", word).unwrap();
                dict.set_item("expected", format!("{expected}")).unwrap();
                dict.set_item("actual", format!("{actual}")).unwrap();
            }
            LiquidityDivergence::TickCallReverted { tick } => {
                dict.set_item("variant", "TickCallReverted").unwrap();
                dict.set_item("tick", tick).unwrap();
            }
            LiquidityDivergence::BitmapCallReverted { word } => {
                dict.set_item("variant", "BitmapCallReverted").unwrap();
                dict.set_item("word", word).unwrap();
            }
        }
        out.push(dict.unbind());
    }
    out
}

// `CancelHandle` lives in `cancel.rs` (extracted) — shared with the Aave
// updater seam. Re-exported here so `run_pool_update`'s `cancel_handle:
// &CancelHandle` resolves + existing `pool::CancelHandle` references work.
pub use crate::cancel::CancelHandle;

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
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.pool")?;
    submod.add_function(wrap_pyfunction!(run_pool_update, &submod)?)?;
    submod.add_function(wrap_pyfunction!(verify_v3_liquidity_map, &submod)?)?;
    submod.add_function(wrap_pyfunction!(verify_v4_liquidity_map, &submod)?)?;
    // `CancelHandle` is registered by `cancel::register_cancel` in `c_api`
    // (shared with the Aave updater seam).
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.pool", &submod)?;
    Ok(())
}

#[cfg(all(test, feature = "auto-initialize"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use degenbot_pool_updater::run::ChunkProgress;

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
                is_final: true,
            });
            sink.report_chunk(&ChunkProgress {
                chain_id: 1,
                chunk_start: 20,
                chunk_end: 29,
                pools_written: 0,
                liquidity_apply_count: 0,
                committed: false,
                is_final: false,
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
            // The `is_final` field round-trips through the PyO3 dict.
            assert!(first
                .get_item("is_final")
                .unwrap()
                .extract::<bool>()
                .unwrap());
            assert!(!second
                .get_item("is_final")
                .unwrap()
                .extract::<bool>()
                .unwrap());
        });
    }
}
