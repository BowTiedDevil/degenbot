//! `PyO3` seam for the `degenbot-pool-updater` chunk loop.
//!
//! Thin `#[pyfunction]` wrapper over
//! [`degenbot_pool_updater::run::run_pool_update`].
//! Arg extraction → GIL release (`py.detach`) → core call → result wrap.
//! No business logic (three-layer architecture, ADR-005). The "Rust is the
//! engine; Python is the cockpit" framing: Python threads the args + a
//! cancel handle; Rust owns the loop, the RPC fetches, the decode, the DB
//! writes, + the per-chunk transaction, and emits its own throttled operator
//! progress lines.
//!
//! # GIL discipline
//!
//! `py.detach(|| core::run_pool_update(...))` releases the GIL across the
//! WHOLE run (long RPC polls hold NO GIL — `rust/AGENTS.md` §GIL); the core
//! never re-acquires it. A Python-side `KeyboardInterrupt` won't
//! pre-empt mid-chunk (the GIL is released); Task 5's signal handler calls
//! [`CancelHandle::cancel`] (the cooperative flag the loop polls between
//! chunks — see [`degenbot_pool_updater::run::run_pool_update`]'s shared-
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

use degenbot_pool_updater::run::{self, NoProgress, ProgressSink, RunError, UpdateReport};

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

/// `degenbot._ffi.pool.run_pool_update(database_path, chain_id, to_block,
/// chunk_size, rpc_url, cancel_handle) -> dict`
///
/// Drive the Rust-owned pool-updater chunk loop for `chain_id`, advancing
/// every active exchange's `last_update_block` to `to_block` (or the chain
/// tip if `to_block is None`). See
/// [`degenbot_pool_updater::run::run_pool_update`] for the §1 three
/// invariants (atomicity / restart-invariance / idempotent re-run) +
/// [`degenbot_pool_updater::run::apply_chunk_writes_on_conn`] for the
/// transaction-semantics core.
///
/// The GIL is released across the WHOLE run (`py.detach`) and the core
/// emits its own throttled operator progress lines — no per-chunk
/// GIL re-acquisition. A Python-side `KeyboardInterrupt` won't pre-empt
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
    cancel_handle,
    verify_chunk = false,
    *,
    verify_all_interval = None,
    verify_all_at_completion = false,
))]
#[expect(clippy::too_many_arguments)]
fn run_pool_update(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    rpc_url: &str,
    cancel_handle: &CancelHandle,
    verify_chunk: bool,
    verify_all_interval: Option<u64>,
    verify_all_at_completion: bool,
) -> PyResult<Py<PyDict>> {
    let path = PathBuf::from(database_path);
    let cancel = cancel_handle.flag.clone();
    // The core emits its own throttled operator progress lines; the
    // seam keeps a silent sink for the core's programmatic `ProgressSink`
    // parameter.
    let progress: Arc<dyn ProgressSink> = Arc::new(NoProgress);

    // GIL released across the WHOLE run. `tokio::runtime::Runtime` is built
    // + `block_on`'d inside the core (on this thread, GIL-free); the seam
    // never re-enters Python.
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

/// `degenbot._ffi.pool.verify_v3_liquidity_map(database_path, rpc_url, chain_id,
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
/// The GIL is released across the whole call (`py.detach`); the `AlloyProvider`
/// is owned internally and the call runs on the ambient tokio runtime — a
/// missing runtime is a typed error, never a per-call runtime build
/// ([`no_ambient_runtime_err`]; mirrors the aave verify seam).
#[pyfunction]
#[pyo3(signature = (database_path, rpc_url, chain_id, pool_address, block_number))]
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
            // resolve the ambient runtime BEFORE any DB work. With no
            // ambient runtime we return a typed error instead of building +
            // dropping a per-call multi-thread runtime (its default
            // worker count is the raw core count — the dead tokio-rt-worker
            // churn source in the live process). The ambient fast path below
            // is unchanged.
            let Ok(handle) = Handle::try_current() else {
                return Err(no_ambient_runtime_err());
            };
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
            let provider = handle.block_on(AlloyProvider::new(rpc_url, 5))?;
            let fut = verify_v3_liquidity_map_on_chain(&provider, addr, &computed, block_number);
            tokio::task::block_in_place(|| handle.block_on(fut))
        })
        .map_err(run_err_to_py)?;

    Ok(divergences_to_dicts(py, &divergences))
}

/// `degenbot._ffi.pool.verify_v4_liquidity_map(database_path, rpc_url, chain_id,
/// pool_hash, pool_manager_address, block_number) -> list[dict]`
///
/// Standalone on-chain-truth verification of a V4 pool's COMMITTED liquidity
/// map via the singleton `PoolManager.extsload(bytes32[])` at `block_number`
/// (read-only — mirrors [`verify_v3_liquidity_map`]). `pool_hash` is the V4
/// `PoolId` (bytes32 hex, `0x…`); `pool_manager_address` is the deployed V4
/// `PoolManager` singleton (the chain has one — it's the V4 exchange's
/// `factory`). Returns the divergence list (empty = GREEN).
///
/// The GIL is released across the whole call; the provider is owned
/// internally and the call runs on the ambient tokio runtime (a missing
/// runtime is a typed error, never a per-call runtime build —
/// [`no_ambient_runtime_err`]).
#[pyfunction]
#[pyo3(signature = (database_path, rpc_url, chain_id, pool_hash, pool_manager_address, block_number))]
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
            // same ambient-runtime policy as the v3 sibling — typed
            // error when no ambient runtime exists; never a per-call
            // multi-thread build.
            let Ok(handle) = Handle::try_current() else {
                return Err(no_ambient_runtime_err());
            };
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
            let provider = handle.block_on(AlloyProvider::new(rpc_url, 5))?;
            let fut = verify_v4_liquidity_map_on_chain(
                &provider,
                pool_manager,
                pool_id,
                &computed,
                block_number,
            );
            tokio::task::block_in_place(|| handle.block_on(fut))
        })
        .map_err(run_err_to_py)?;

    Ok(divergences_to_dicts(py, &divergences))
}

/// Encode a [`degenbot_pool_updater::LiquidityDivergence`] list as a list of
/// dicts (empty = GREEN). Each dict carries `variant` + the named fields for
/// bisect-able triage (the same shape the pre-commit gate surfaces).
#[expect(clippy::unwrap_used)] // dict.set_item can't fail on these literal values
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
            LiquidityDivergence::TickPresence {
                tick,
                stored,
                observed,
            } => {
                dict.set_item("variant", "TickPresence").unwrap();
                dict.set_item("tick", tick).unwrap();
                dict.set_item("stored", stored).unwrap();
                dict.set_item("observed", observed).unwrap();
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

/// The typed error for a Python-called verify path with no ambient tokio
/// runtime: these seams used to build + drop a full
/// multi-thread runtime per call (default worker count = the raw core
/// count), churning dead tokio-rt-worker threads in the live process. They
/// now run only on the caller's ambient runtime and fail loudly here
/// otherwise. Uses the existing `RunError::Runtime(io::Error)` variant so
/// `run_err_to_py` maps it to `ValueError` like every other failure.
fn no_ambient_runtime_err() -> RunError {
    RunError::Runtime(std::io::Error::other(
        "no ambient tokio runtime: refusing to build a per-call multi-thread runtime; run under the shared degenbot-core ambient runtime",
    ))
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
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// with NO ambient tokio runtime, the verify seams must fail with
    /// the typed no-ambient-runtime error INSTEAD of building + dropping a
    /// per-call multi-thread runtime (its default worker count is the
    /// raw core count — the dead tokio-rt-worker churn seen in the live
    /// process). An empty in-memory DB + an unroutable RPC keep the tests
    /// offline; the runtime-policy check fires before any DB work.
    #[test]
    fn verify_v3_no_ambient_runtime_errors_instead_of_spawning() {
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "test thread must have no ambient tokio runtime"
        );
        Python::attach(|py| {
            let err = verify_v3_liquidity_map(
                py,
                ":memory:",
                "http://127.0.0.1:1",
                1,
                "0x0000000000000000000000000000000000000001",
                1,
            )
            .unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("no ambient tokio runtime"),
                "expected the typed no-ambient-runtime error, got: {msg}"
            );
            assert!(
                !msg.contains("database error"),
                "must fail on runtime policy before DB work, got: {msg}"
            );
        });
    }

    /// v4 sibling of the v3 no-ambient-runtime pin.
    #[test]
    fn verify_v4_no_ambient_runtime_errors_instead_of_spawning() {
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "test thread must have no ambient tokio runtime"
        );
        Python::attach(|py| {
            let err = verify_v4_liquidity_map(
                py,
                ":memory:",
                "http://127.0.0.1:1",
                1,
                "0x0101010101010101010101010101010101010101010101010101010101010101",
                "0x0000000000000000000000000000000000000002",
                1,
            )
            .unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("no ambient tokio runtime"),
                "expected the typed no-ambient-runtime error, got: {msg}"
            );
            assert!(
                !msg.contains("database error"),
                "must fail on runtime policy before DB work, got: {msg}"
            );
        });
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
}
