//! `PyO3` seam for the `degenbot-aave` chunk loop (epic `AZGJUN`,
//! task `5XNTC5`).
//!
//! Thin `#[pyfunction]` wrapper over
//! [`degenbot_aave::run::run_aave_update`] (the 6SWY4R core). Arg
//! extraction → GIL release (`py.detach`) → core call → result wrap. No
//! business logic (three-layer architecture, ADR-005). The "Rust is the
//! engine; Python is the cockpit" framing: Python threads the args + a
//! cancel handle; Rust owns the loop, the RPC fetches, the decode, the DB
//! writes, + the per-chunk transaction, and emits its own throttled operator
//! progress lines (Q5IKHX).
//!
//! # GIL discipline
//!
//! `py.detach(|| core::run_aave_update(...))` releases the GIL across the
//! WHOLE run (long RPC polls hold NO GIL — `rust/AGENTS.md` §GIL); the core
//! never re-acquires it. A Python-side `KeyboardInterrupt` won't
//! pre-empt mid-chunk (the GIL is released); the CLI signal handler calls
//! [`crate::cancel::CancelHandle::cancel`] (the cooperative flag the loop
//! polls between chunks).
//!
//! # Owned-runtime constraint (D2)
//!
//! [`degenbot_aave::run::run_aave_update`] owns its
//! `tokio::runtime::Runtime`. Calling it from within an existing tokio
//! runtime panics ("Cannot start a runtime from within a runtime"). The CLI
//! driver runs `run_aave_update` from a worker thread with NO ambient tokio
//! runtime. See [`crate::pool`] for the same constraint.

use std::path::PathBuf;
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};

use degenbot_aave::{
    activate_aave_market as core_activate_aave_market,
    deactivate_aave_market as core_deactivate_aave_market, run_aave_update as core_run_aave_update,
    verify::{
        cleanup_zero_balance_positions_on_conn, verify_all_positions_on_conn,
        verify_touched_positions_on_conn,
    },
    AaveUpdateReport, NoProgress, ProgressSink, RunError,
};

use crate::cancel::CancelHandle;

/// Build the `AaveUpdateReport` return `dict` (matches the pool seam's
/// `update_report_to_dict` idiom).
///
/// Shape: `{"chain_id": int, "market_id": int, "from_block": int,
/// "to_block": int, "chunks_committed": int, "total_events_applied": int}`.
fn aave_report_to_dict(py: Python<'_>, r: &AaveUpdateReport) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("chain_id", r.chain_id)?;
    dict.set_item("market_id", r.market_id)?;
    dict.set_item("from_block", r.from_block)?;
    dict.set_item("to_block", r.to_block)?;
    dict.set_item("chunks_committed", r.chunks_committed)?;
    dict.set_item("total_events_applied", r.total_events_applied)?;
    Ok(dict.unbind())
}

/// `degenbot._ffi.aave.run_aave_update(database_path, chain_id, market_id, to_block,
/// chunk_size, rpc_url, cancel_handle, verify_chunk,
/// max_chunks) -> dict`
///
/// Drive the Rust-owned Aave V3 updater chunk loop for `market_id`, advancing
/// `aave_v3_markets.last_update_block` to `to_block` (or the chain tip if
/// `to_block is None`). See
/// [`degenbot_aave::run::run_aave_update`] for the §3.4 atomicity
/// invariant (one `Transaction` per chunk; failure mid-chunk → rollback →
/// `last_update_block` unchanged → restart re-processes clean).
///
/// The GIL is released across the WHOLE run (`py.detach`) and the core
/// emits its own throttled operator progress lines (Q5IKHX) — no per-chunk
/// GIL re-acquisition. A Python-side `KeyboardInterrupt` won't pre-empt
/// mid-chunk; the SIGINT handler calls `cancel_handle.cancel()` (the
/// cooperative flag the loop polls between chunks — §3.3 interrupt contract:
/// SIGINT between chunks → honored immediately; SIGINT mid-chunk → the chunk
/// completes atomically first).
///
/// # Args
///
/// - `database_path` — the writeable `DegenbotDb` path.
/// - `chain_id` — the chain.
/// - `market_id` — the `aave_v3_markets.id` to advance.
/// - `to_block` — `int` to advance to a specific block; `None` for the tip.
/// - `chunk_size` — blocks per chunk.
/// - `rpc_url` — the HTTP RPC endpoint.
/// - `cancel_handle` — a [`CancelHandle`] (shared with `run_pool_update`);
///   a `signal.SIGINT` handler calls `.cancel()`.
/// - `verify_chunk` — if `True`, run pre-commit verification on each chunk:
///   Rust calls `verify_touched_positions_on_conn` on the uncommitted
///   transaction BEFORE `tx.commit()`. A divergence drops the tx (rollback)
///   so `last_update_block` does NOT advance + the next run re-processes the
///   same chunk. If `False`, verification is skipped (chunks commit without
///   checking).
/// - `max_chunks` — `None` to advance to `to_block`/tip; `Some(n)` to stop
///   after committing `n` chunks (one-chunk mode). `last_update_block` is
///   advanced to the last committed chunk's end, so the next run resumes
///   from there. Previously the Python `--one-chunk` flag was downgraded to
///   a warning by the cutover shell; this restores first-class support.
///
/// # Returns
///
/// A `dict` `{chain_id, market_id, from_block, to_block, chunks_committed,
/// total_events_applied}`.
///
/// # Raises
///
/// `ValueError` on a DB / RPC / config-dispatch / parse failure (the in-flight
/// chunk is rolled back before returning; committed chunks stay durable).
/// `RuntimeError` if cancelled.
/// `AssertionError` if `verify_chunk=True` and pre-commit verification found
/// divergences (the in-flight chunk is rolled back — the chunk's data is NOT
/// committed; `last_update_block` did NOT advance).
///
/// # Runtime nesting
///
/// Must NOT be called from within an existing `tokio` runtime (the core owns
/// its runtime; nesting panics). See the module docs.
#[pyfunction]
#[pyo3(signature = (
    database_path,
    chain_id,
    market_id,
    to_block,
    chunk_size,
    rpc_url,
    cancel_handle,
    verify_chunk=false,
    max_chunks=None,
    *,
    verify_all_interval=None,
    verify_all_at_completion=false,
))]
#[expect(clippy::too_many_arguments)]
fn run_aave_update(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    market_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    rpc_url: &str,
    cancel_handle: &CancelHandle,
    verify_chunk: bool,
    max_chunks: Option<usize>,
    verify_all_interval: Option<u64>,
    verify_all_at_completion: bool,
) -> PyResult<Py<PyDict>> {
    let path = PathBuf::from(database_path);
    let cancel = cancel_handle.flag.clone();
    // The core emits its own throttled operator progress lines (Q5IKHX); the
    // seam keeps a silent sink for the core's programmatic `ProgressSink`
    // parameter.
    let progress: Arc<dyn ProgressSink> = Arc::new(NoProgress);

    // GIL released across the WHOLE run. `tokio::runtime::Runtime` is built
    // + `block_on`'d inside the core (on this thread, GIL-free); the seam
    // never re-enters Python.
    let report = py
        .detach(move || {
            core_run_aave_update(
                &path,
                chain_id,
                market_id,
                to_block,
                chunk_size,
                rpc_url,
                cancel,
                progress,
                verify_chunk,
                verify_all_interval,
                verify_all_at_completion,
                max_chunks,
            )
        })
        .map_err(run_err_to_py)?;

    aave_report_to_dict(py, &report)
}

/// The typed error for a Python-called verify path with no ambient tokio
/// runtime (VJGZJ2): these seams used to build + drop a full
/// multi-thread runtime per call (default worker count = the raw core
/// count), churning dead tokio-rt-worker threads in the live process. They
/// now run only on the caller's ambient runtime and fail loudly here
/// otherwise. Uses the existing `RunError::Runtime(io::Error)` variant so
/// `run_err_to_py` maps it to `ValueError` like every other failure.
fn no_ambient_runtime_err() -> RunError {
    RunError::Runtime(std::io::Error::other(
        "no ambient tokio runtime: refusing to build a per-call multi-thread runtime (VJGZJ2); run under the shared degenbot-core ambient runtime",
    ))
}

/// Map a [`RunError`] to a Python exception. Mirrors the pool seam's
/// `run_err_to_py`: `RunError::Cancelled` → `RuntimeError` (so the driver
/// distinguishes a cooperative cancel from a failure);
/// `RunError::Verification` → `AssertionError` (formatted with divergence
/// details — the in-flight chunk was rolled back, `last_update_block` did
/// NOT advance); everything else → `ValueError`.
fn run_err_to_py(err: RunError) -> PyErr {
    use pyo3::exceptions::{PyAssertionError, PyRuntimeError, PyValueError};
    match err {
        RunError::Cancelled => PyRuntimeError::new_err("run_aave_update cancelled by cancel flag"),
        RunError::Verification {
            chunk_start,
            chunk_end,
            divergences,
        } => {
            use std::fmt::Write as _;
            let mut lines = format!(
                "{} divergence(s) at chunk {chunk_start}-{chunk_end}:",
                divergences.len()
            );
            for d in &divergences {
                let kind = match d.kind {
                    degenbot_aave::verify::PositionKind::Collateral => "collateral",
                    degenbot_aave::verify::PositionKind::Debt => "debt",
                };
                let field = match d.field {
                    degenbot_aave::verify::DivergenceField::Balance => "balance",
                    degenbot_aave::verify::DivergenceField::LastIndex => "last_index",
                };
                let _ = write!(
                    lines,
                    "\n  {kind} {field}: {:?} expected={} actual={}",
                    d.user_address, d.expected, d.actual
                );
            }
            PyAssertionError::new_err(lines)
        }
        other => PyValueError::new_err(other.to_string()),
    }
}

/// `degenbot._ffi.aave.verify_touched_positions_on_chain(database_path, rpc_url,
/// market_id, chain_id, block_number, touched_users=None) -> list[dict]`
///
/// Minimal-slice Rust port of Python's
/// `verification.py::verify_scaled_token_positions` (collateral + debt
/// dimensions only).
///
/// Loads `aave_v3_collateral_positions` + `aave_v3_debt_positions` from
/// `database_path` (filtered by `touched_users` when given) joined to the
/// user address + the aToken/vToken erc20 token address, then for each
/// position (skipping `DEAD_ADDRESS` / `ZERO_ADDRESS` users, mirroring the
/// Python) RPC `scaledBalanceOf(user)` + `getPreviousIndex(user)` on the
/// aToken / vToken at `block_number` + asserts equality with the DB `balance`
/// + `last_index`.
///
/// Returns a list of divergence dicts (empty = GREEN). Each dict has:
/// `kind`, `position_id`, `user_address`, `token_address`, `block_number`,
/// `field`, `expected`, `actual` — the same shape the JGQHBX drive harness's
/// compare divergences emit, so it's bisect-able.
///
/// Per-position `eth_call`s — acceptable for the touched-users-per-chunk case
/// (small set). Multicall3 batching for the market-wide verify is the natural
/// extension (BE474R-full, post-HLYWI6).
///
/// The GIL is released across the whole call (`py.detach`); the orchestrator
/// owns its tokio runtime + `AlloyProvider` internally. Zero SQL writes.
///
/// # Args
///
/// - `database_path` — the `DegenbotDb` path (opened read-only).
/// - `rpc_url` — the HTTP RPC endpoint.
/// - `market_id` — the `aave_v3_markets.id` to verify.
/// - `chain_id` — reserved for parity with `run_aave_update`; not used in
///   the verify path (the writer-state already filtered to `market_id`).
/// - `block_number` — the block to verify against (`chunk_end` in the
///   per-chunk gate).
/// - `touched_users` — `None` (default) verifies ALL positions;
///   `["0x...", ...]` verifies only those users (the JGQHBX drive harness's
///   per-chunk path passes the `touched_user_addresses` from
///   `run_aave_update`'s progress dict for efficiency).
#[pyfunction]
#[pyo3(signature = (database_path, rpc_url, market_id, chain_id, block_number, touched_users=None))]
fn verify_touched_positions_on_chain(
    py: Python<'_>,
    database_path: &str,
    rpc_url: &str,
    market_id: i64,
    #[expect(unused_variables)] chain_id: i64,
    block_number: u64,
    touched_users: Option<Vec<String>>,
) -> PyResult<Vec<Py<PyDict>>> {
    use degenbot_db::DegenbotDb;
    use degenbot_rpc::provider::AlloyProvider;

    let path = PathBuf::from(database_path);
    // Parse the optional touched_users filter into Addresses.
    let touched: Option<Vec<alloy::primitives::Address>> =
        touched_users.map(|addrs| addrs.iter().filter_map(|s| s.parse().ok()).collect());

    let divergences = py
        .detach(move || {
            use tokio::runtime::Handle;
            // VJGZJ2: resolve the ambient runtime BEFORE any DB work. With no
            // ambient runtime we return a typed error instead of building +
            // dropping a per-call multi-thread runtime (its default
            // worker count is the raw core count — the dead tokio-rt-worker
            // churn source in the live process). The ambient fast path below
            // is unchanged.
            let Ok(handle) = Handle::try_current() else {
                return Err(no_ambient_runtime_err());
            };

            // Lock the DB handle for a read-only connection (verify uses no
            // SQL writes; the lock guarantees no concurrent mutator inside
            // the same `DegenbotDb` handle — the JGQHBX drive harness calls
            // from a SEPARATE connection from `run_aave_update`'s writer).
            let db = DegenbotDb::open(&path)?.0;
            let conn = db.lock();
            let touched_ref = touched.as_deref();
            // Runtime strategy: the ambient handle is resolved up front (the
            // VJGZJ2 policy — a missing runtime errors, never a per-call
            // build). Invoking from inside an existing runtime (e.g. a
            // caller driving verification from the updater worker — context
            // set + multi-thread) uses `block_in_place` + `handle.block_on`
            // (avoids the "creating runtime within runtime" panic). The
            // `AlloyProvider` is
            // constructed inside that runtime's context to avoid its
            // internals' runtime-handle binding.
            let provider = handle.block_on(AlloyProvider::new(rpc_url, 5))?;
            let fut = verify_touched_positions_on_conn(
                &conn,
                &provider,
                market_id,
                block_number,
                touched_ref,
            );
            tokio::task::block_in_place(|| handle.block_on(fut))
        })
        .map_err(run_err_to_py)?;

    // Build the divergence dicts.
    let mut out: Vec<Py<PyDict>> = Vec::with_capacity(divergences.len());
    for d in divergences {
        let dict = PyDict::new(py);
        dict.set_item(
            "kind",
            match d.kind {
                degenbot_aave::verify::PositionKind::Collateral => "collateral",
                degenbot_aave::verify::PositionKind::Debt => "debt",
            },
        )?;
        dict.set_item("position_id", d.position_id)?;
        dict.set_item("user_address", format!("{:?}", d.user_address))?;
        dict.set_item("token_address", format!("{:?}", d.token_address))?;
        dict.set_item("block_number", d.block_number)?;
        dict.set_item(
            "field",
            match d.field {
                degenbot_aave::verify::DivergenceField::Balance => "balance",
                degenbot_aave::verify::DivergenceField::LastIndex => "last_index",
            },
        )?;
        dict.set_item("expected", format!("{}", d.expected))?;
        dict.set_item("actual", format!("{}", d.actual))?;
        out.push(dict.unbind());
    }
    Ok(out)
}

/// `degenbot._ffi.aave.verify_all_positions_on_chain(database_path, rpc_url,
/// market_id, chain_id, block_number, touched_users=None) -> list[dict]`
///
/// Full on-chain-truth verification — the Rust port of Python's
/// `verify_all_positions` (verification.py) + `verify_stk_aave_balances` +
/// `verify_gho_discount_amounts` (`db_verification.py`). Runs all 4 checks:
/// 1. Collateral scaled-token balance + `last_index`.
/// 2. Debt scaled-token balance + `last_index`.
/// 3. stkAAVE balance (`balanceOf` on the discount token).
/// 4. GHO discount percent (`getDiscountPercent` on the GHO vToken,
///    with the revision-based skip guard).
///
/// Returns a list of divergence dicts (empty = GREEN). Each dict has a
/// `check` field (`"scaled_token"`, `"stk_aave_balance"`, or
/// `"gho_discount"`) plus the relevant fields for that check type.
///
/// The GIL is released across the whole call. Zero SQL writes —
/// [`cleanup_zero_balance_positions`] is the separate write companion.
///
/// # Args
///
/// - `database_path` — the `DegenbotDb` path (opened read-only).
/// - `rpc_url` — the HTTP RPC endpoint.
/// - `market_id` — the `aave_v3_markets.id` to verify.
/// - `chain_id` — the chain ID (needed to resolve the GHO asset row).
/// - `block_number` — the block to verify against.
/// - `touched_users` — `None` (default) verifies ALL positions/users;
///   `Some(["0x...", ...])` verifies only those users.
#[pyfunction]
#[pyo3(signature = (database_path, rpc_url, market_id, chain_id, block_number, touched_users=None))]
fn verify_all_positions_on_chain(
    py: Python<'_>,
    database_path: &str,
    rpc_url: &str,
    market_id: i64,
    chain_id: i64,
    block_number: u64,
    touched_users: Option<Vec<String>>,
) -> PyResult<Vec<Py<PyDict>>> {
    use degenbot_db::DegenbotDb;
    use degenbot_rpc::provider::AlloyProvider;

    let path = PathBuf::from(database_path);
    let touched: Option<Vec<alloy::primitives::Address>> =
        touched_users.map(|addrs| addrs.iter().filter_map(|s| s.parse().ok()).collect());

    let divergences = py
        .detach(move || {
            use tokio::runtime::Handle;
            // VJGZJ2: same ambient-runtime policy as the touched-positions
            // sibling — typed error when no ambient runtime exists; never a
            // per-call multi-thread build.
            let Ok(handle) = Handle::try_current() else {
                return Err(no_ambient_runtime_err());
            };

            let db = DegenbotDb::open(&path)?.0;
            let conn = db.lock();
            let touched_ref = touched.as_deref();

            // Runtime strategy: the ambient handle is resolved up front (the
            // VJGZJ2 policy — a missing runtime errors, never a per-call
            // build). The `AlloyProvider` is constructed inside that
            // runtime's context to avoid its internals' runtime-handle
            // binding.
            let provider = handle.block_on(AlloyProvider::new(rpc_url, 5))?;
            let fut = verify_all_positions_on_conn(
                &conn,
                &provider,
                market_id,
                chain_id,
                block_number,
                touched_ref,
            );
            tokio::task::block_in_place(|| handle.block_on(fut))
        })
        .map_err(run_err_to_py)?;

    let mut out: Vec<Py<PyDict>> = Vec::with_capacity(divergences.len());
    for d in divergences {
        let dict = PyDict::new(py);
        match d {
            degenbot_aave::verify::VerificationDivergence::ScaledToken(p) => {
                dict.set_item("check", "scaled_token")?;
                dict.set_item(
                    "kind",
                    match p.kind {
                        degenbot_aave::verify::PositionKind::Collateral => "collateral",
                        degenbot_aave::verify::PositionKind::Debt => "debt",
                    },
                )?;
                dict.set_item("position_id", p.position_id)?;
                dict.set_item("user_address", format!("{:?}", p.user_address))?;
                dict.set_item("token_address", format!("{:?}", p.token_address))?;
                dict.set_item("block_number", p.block_number)?;
                dict.set_item(
                    "field",
                    match p.field {
                        degenbot_aave::verify::DivergenceField::Balance => "balance",
                        degenbot_aave::verify::DivergenceField::LastIndex => "last_index",
                    },
                )?;
                dict.set_item("expected", format!("{}", p.expected))?;
                dict.set_item("actual", format!("{}", p.actual))?;
            }
            degenbot_aave::verify::VerificationDivergence::StkAaveBalance {
                user_id,
                user_address,
                token_address,
                block_number,
                expected,
                actual,
            } => {
                dict.set_item("check", "stk_aave_balance")?;
                dict.set_item("user_id", user_id)?;
                dict.set_item("user_address", format!("{user_address:?}"))?;
                dict.set_item("token_address", format!("{token_address:?}"))?;
                dict.set_item("block_number", block_number)?;
                dict.set_item("expected", format!("{expected}"))?;
                dict.set_item("actual", format!("{actual}"))?;
            }
            degenbot_aave::verify::VerificationDivergence::GhoDiscount {
                user_id,
                user_address,
                token_address,
                block_number,
                expected,
                actual,
            } => {
                dict.set_item("check", "gho_discount")?;
                dict.set_item("user_id", user_id)?;
                dict.set_item("user_address", format!("{user_address:?}"))?;
                dict.set_item("token_address", format!("{token_address:?}"))?;
                dict.set_item("block_number", block_number)?;
                dict.set_item("expected", expected)?;
                dict.set_item("actual", actual)?;
            }
        }
        out.push(dict.unbind());
    }
    Ok(out)
}

/// `degenbot._ffi.aave.cleanup_zero_balance_positions(database_path, market_id)`
///
/// Delete all zero-balance collateral + debt positions for `market_id`.
/// Mirrors the Python `cleanup_zero_balance_positions` (verification.py:19).
/// Opens the DB for writes, deletes, + commits. The GIL is released across
/// the call.
#[pyfunction]
fn cleanup_zero_balance_positions(
    py: Python<'_>,
    database_path: &str,
    market_id: i64,
) -> PyResult<()> {
    use degenbot_db::{DbError, DegenbotDb};
    let path = PathBuf::from(database_path);
    py.detach(move || -> Result<(), RunError> {
        let (db, _state) = DegenbotDb::open_for_writes(&path)?;
        {
            let mut guard = db.lock();
            let tx = guard.transaction().map_err(DbError::from)?;
            cleanup_zero_balance_positions_on_conn(&tx, market_id)?;
            tx.commit().map_err(DbError::from)?;
        }
        Ok(())
    })
    .map_err(run_err_to_py)
}

/// `degenbot._ffi.aave.activate_aave_market(database_path, chain_id,
/// pool_address_provider, gho_token_address, rpc_url) -> dict`
///
/// Seed (or re-activate) an Aave V3 market — the ONE-TIME setup the chunk
/// loop's `run_aave_update` bootstraps from. Rust-owned replacement for the
/// Python `activate_ethereum_aave_v3` (commands.py) — the last ORM writer on
/// the Aave path after the §4.2 retirement (CZM7TI). Activates the market,
/// inserts the `POOL_ADDRESS_PROVIDER` contract row, + seeds the GHO
/// `erc20_tokens` + `aave_gho_tokens` rows, all in ONE transaction.
///
/// The GIL is released across the WHOLE call (`py.detach`) — the core owns
/// its tokio runtime + does the RPC fetches (`getMarketId()` + GHO metadata)
/// + the DB writes internally.
///
/// # Returns
///
/// A `dict` `{market_id, market_name, created}` — `market_id` is the
/// `aave_v3_markets.id` to pass to `run_aave_update`; `created` is `True` if
/// the market was newly created, `False` if it pre-existed (re-activation).
///
/// # Raises
///
/// `ValueError` on a DB / RPC / address-parse failure.
#[pyfunction]
fn activate_aave_market(
    py: Python<'_>,
    database_path: &str,
    chain_id: i64,
    pool_address_provider: &str,
    gho_token_address: &str,
    rpc_url: &str,
) -> PyResult<Py<PyDict>> {
    let path = PathBuf::from(database_path);
    let result = py
        .detach(move || {
            core_activate_aave_market(
                &path,
                chain_id,
                pool_address_provider,
                gho_token_address,
                rpc_url,
            )
        })
        .map_err(run_err_to_py)?;

    let dict = PyDict::new(py);
    dict.set_item("market_id", result.market_id)?;
    dict.set_item("market_name", result.market_name)?;
    dict.set_item("created", result.created)?;
    Ok(dict.unbind())
}

/// `degenbot._ffi.aave.deactivate_aave_market(database_path, market_id) -> None`
///
/// Set `active = False` for `market_id`. Rust-owned replacement for the
/// Python `deactivate_mainnet_aave_v3` (commands.py). The GIL is released
/// across the call.
///
/// # Raises
///
/// `ValueError` if `market_id` doesn't exist or on a DB failure.
#[pyfunction]
fn deactivate_aave_market(py: Python<'_>, database_path: &str, market_id: i64) -> PyResult<()> {
    let path = PathBuf::from(database_path);
    py.detach(move || core_deactivate_aave_market(&path, market_id))
        .map_err(run_err_to_py)?;
    Ok(())
}

/// Register the aave-updater seam on `m` (feature = "aave-updater"). Mirrors
/// `pool::add_pool_module`. `CancelHandle` is registered separately by
/// `cancel::register_cancel` (shared).
///
/// # Errors
///
/// Returns a [`PyErr`] if the `add_function` call fails (a name collision);
/// propagated unchanged to the `#[pymodule]` caller.
pub fn add_aave_updater_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.aave")?;
    submod.add_function(wrap_pyfunction!(run_aave_update, &submod)?)?;
    submod.add_function(wrap_pyfunction!(
        verify_touched_positions_on_chain,
        &submod
    )?)?;
    submod.add_function(wrap_pyfunction!(verify_all_positions_on_chain, &submod)?)?;
    submod.add_function(wrap_pyfunction!(cleanup_zero_balance_positions, &submod)?)?;
    submod.add_function(wrap_pyfunction!(activate_aave_market, &submod)?)?;
    submod.add_function(wrap_pyfunction!(deactivate_aave_market, &submod)?)?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.aave", &submod)?;
    Ok(())
}

#[cfg(all(test, feature = "auto-initialize"))]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// VJGZJ2: with NO ambient tokio runtime, the aave verify seams must fail
    /// with the typed no-ambient-runtime error INSTEAD of building + dropping
    /// a per-call multi-thread runtime (default worker count = raw core
    /// count — the dead tokio-rt-worker churn source). An empty in-memory DB +
    /// an unroutable RPC keep the tests offline; the runtime-policy check
    /// fires before any DB work.
    #[test]
    fn verify_touched_no_ambient_runtime_errors_instead_of_spawning() {
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "test thread must have no ambient tokio runtime"
        );
        Python::attach(|py| {
            let err = verify_touched_positions_on_chain(
                py,
                ":memory:",
                "http://127.0.0.1:1",
                1,
                1,
                1,
                None,
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

    /// VJGZJ2: full-verify sibling of the touched-positions no-ambient pin.
    #[test]
    fn verify_all_no_ambient_runtime_errors_instead_of_spawning() {
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "test thread must have no ambient tokio runtime"
        );
        Python::attach(|py| {
            let err =
                verify_all_positions_on_chain(py, ":memory:", "http://127.0.0.1:1", 1, 1, 1, None)
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

    /// `aave_report_to_dict` produces the six-key report dict the `.pyi`
    /// contract promises (the `run_aave_update` return shape).
    #[test]
    fn aave_report_dict_shape() {
        Python::attach(|py| {
            let r = AaveUpdateReport {
                chain_id: 1,
                market_id: 42,
                from_block: 10,
                to_block: 99,
                chunks_committed: 3,
                total_events_applied: 17,
            };
            let d = aave_report_to_dict(py, &r).unwrap();
            let bound = d.bind(py);
            for k in [
                "chain_id",
                "market_id",
                "from_block",
                "to_block",
                "chunks_committed",
                "total_events_applied",
            ] {
                assert!(bound.contains(k).unwrap(), "missing key {k}");
            }
            assert_eq!(
                bound
                    .get_item("market_id")
                    .unwrap()
                    .unwrap()
                    .extract::<i64>()
                    .unwrap(),
                42
            );
            assert_eq!(
                bound
                    .get_item("total_events_applied")
                    .unwrap()
                    .unwrap()
                    .extract::<usize>()
                    .unwrap(),
                17
            );
        });
    }
}
