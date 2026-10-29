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
//! WHOLE run (long RPC polls hold NO GIL); the core
//! never re-acquires it. A Python-side `KeyboardInterrupt` won't
//! pre-empt mid-chunk (the GIL is released); Task 5's signal handler calls
//! [`CancelHandle::cancel`] (the cooperative flag the loop polls between
//! chunks — see [`degenbot_pool_updater::run::run_pool_update`]'s shared-
//! runtime constraint + the interrupt contract).
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

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use alloy::primitives::{Address, B256};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use degenbot_pool_updater::run::{self, NoProgress, RunError, UpdateReport};

/// Build the `UpdateReport` return `dict` (matches the `db_heal_database`
/// `dict`-return idiom from `67691f7c`).
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
/// [`degenbot_pool_updater::run::run_pool_update`] for the
/// chunk-atomicity invariants (`docs/architecture/chunk-atomicity.md`;
/// atomicity / restart-invariance / idempotent re-run) +
/// [`degenbot_pool_updater::run::apply_chunk_writes_on_conn`] for the
/// transaction-semantics core.
///
/// The GIL is released across the WHOLE run (`py.detach`) and the core
/// emits its own throttled operator progress lines — no per-chunk
/// GIL re-acquisition. A Python-side `KeyboardInterrupt` won't pre-empt
/// mid-chunk; Task 5's SIGINT handler calls `cancel_handle.cancel()` (the
/// cooperative flag the loop polls between chunks — the interrupt contract
/// (`docs/architecture/chunk-atomicity.md`):
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
/// - `chunk_size` — blocks per chunk (the batch the chunk's single
///   transaction covers).
/// - `max_block_range` — the widest block range a single `eth_getLogs`
///   request may span (the `MAX_BLOCKS_PER_REQUEST` clamp, independent of
///   `chunk_size`); the Python updater's per-request clamp.
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
    max_block_range = 10000,
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
    max_block_range: u64,
) -> PyResult<Py<PyDict>> {
    let path = PathBuf::from(database_path);
    let cancel = cancel_handle.flag.clone();

    // GIL released across the WHOLE run — including the live transport's
    // build, which runs on the runtime the seam edge owns. The core only
    // injects the built provider, and the seam never re-enters Python.
    let report = py
        .detach(move || {
            // The thin live-provider wrapper (ADR-068 D5): the shell builds
            // the live transport from `rpc_url` and the core only injects it.
            let provider = crate::provider_seam::drive(crate::provider_seam::live_provider(
                rpc_url,
                crate::provider_seam::UPDATER_MAX_RETRIES,
            ))
            .map_err(RunError::from)?;
            run_pool_update_injected(
                &path,
                chain_id,
                to_block,
                chunk_size,
                max_block_range,
                provider,
                cancel,
                verify_chunk,
                verify_all_interval,
                verify_all_at_completion,
            )
        })
        .map_err(run_err_to_py)?;

    update_report_to_dict(py, &report)
}

/// The pool-updater seam's provider-injected entry (ADR-068 D5): drive the
/// core chunk loop with an ALREADY-BUILT provider.
///
/// [`run_pool_update`] is the operator's `rpc_url` adapter — it builds the
/// live transport through [`crate::provider_seam`] and lands here. A
/// `CassetteReplayTransport`-backed provider lands here too, which is what
/// makes the committed capture corpus reachable from this seam at all.
///
/// The core emits its own throttled operator progress lines, so the seam
/// keeps a silent sink for the core's programmatic `ProgressSink`.
#[expect(clippy::too_many_arguments)]
fn run_pool_update_injected(
    database_path: &Path,
    chain_id: i64,
    to_block: Option<u64>,
    chunk_size: u64,
    max_block_range: u64,
    provider: degenbot_rpc::provider::AlloyProvider,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    verify_chunk: bool,
    verify_all_interval: Option<u64>,
    verify_all_at_completion: bool,
) -> Result<UpdateReport, RunError> {
    run::run_pool_update(
        database_path,
        chain_id,
        to_block,
        chunk_size,
        max_block_range,
        provider,
        cancel,
        Arc::new(NoProgress),
        verify_chunk,
        verify_all_interval,
        verify_all_at_completion,
    )
}

/// The pool identity a verify call is keyed by, resolved from the `family`
/// argument plus the raw argument strings.
enum VerifyIdentity {
    /// A V3 pool contract, plus the checksummed key its committed row is
    /// stored under.
    V3 {
        /// The pool contract.
        pool_address: Address,
        /// The committed `pools` row key.
        db_key: String,
    },
    /// A V4 pool: its `PoolManager` singleton, its `PoolId`, plus the caller's
    /// `pool_id` text. The committed `pool_hash` column stores that text
    /// verbatim, so re-formatting the parsed value would miss row spellings
    /// the lookup accepts.
    V4 {
        /// The V4 `PoolManager` singleton.
        pool_manager: Address,
        /// The V4 pool identity.
        pool_id: B256,
        /// The committed `pool_hash` row key.
        db_key: String,
    },
}

/// Resolve a verify call's identity, refusing a key that names no pool before
/// any DB open or provider build.
fn parse_verify_identity(
    family: &str,
    address_or_manager: &str,
    pool_id: Option<&str>,
) -> PyResult<VerifyIdentity> {
    use pyo3::exceptions::PyValueError;

    match family {
        "v3" => {
            if pool_id.is_some() {
                return Err(PyValueError::new_err(
                    "pool_id names a V4 pool; pass None with family \"v3\"",
                ));
            }
            let pool_address: Address = address_or_manager
                .parse()
                .map_err(|e| PyValueError::new_err(format!("bad pool_address: {e}")))?;
            Ok(VerifyIdentity::V3 {
                pool_address,
                db_key: pool_address.to_checksum(None),
            })
        }
        "v4" => {
            let pool_id_hex = pool_id.ok_or_else(|| {
                PyValueError::new_err("pool_id is required with family \"v4\" (bytes32 hex)")
            })?;
            let pool_manager: Address = address_or_manager
                .parse()
                .map_err(|e| PyValueError::new_err(format!("bad pool_manager_address: {e}")))?;
            let pool_id = B256::from_str(pool_id_hex.strip_prefix("0x").unwrap_or(pool_id_hex))
                .map_err(|e| PyValueError::new_err(format!("bad pool_id: {e}")))?;
            Ok(VerifyIdentity::V4 {
                pool_manager,
                pool_id,
                db_key: pool_id_hex.to_string(),
            })
        }
        other => Err(PyValueError::new_err(format!(
            "unknown family {other:?}; expected \"v3\" or \"v4\""
        ))),
    }
}

/// `degenbot._ffi.pool.verify_liquidity_map(database_path, rpc_url, chain_id,
/// family, address_or_manager, pool_id, block_number) -> list[dict]`
///
/// Standalone on-chain-truth verification of one pool's COMMITTED liquidity
/// map (read-only — no SQL writes, no event apply). Opens the DB, fetches the
/// pool's `liquidity_positions` rows, builds the in-memory map, + compares
/// EVERY tick + (derived) bitmap word against chain state at `block_number`:
/// `IUniswapV3Pool.ticks(int24)` / `tickBitmap(int16)` batched through
/// Multicall3 for `family="v3"`, the singleton
/// `PoolManager.extsload(bytes32[])` for `family="v4"`. Returns the divergence
/// list (empty = GREEN — the DB matches the chain at `block_number`).
///
/// This is the ad-hoc / spot-check sibling of the pre-commit gate
/// (`run_pool_update(verify_chunk=True)`); the gate runs the SAME core verify
/// BEFORE the write commits, while this reads the already-committed state.
///
/// The GIL is released across the whole call (`py.detach`); the live
/// `AlloyProvider` is built through [`crate::provider_seam`] and the call
/// runs on the ambient tokio runtime — a missing runtime is a typed error,
/// never a per-call runtime build
/// ([`crate::provider_seam::require_ambient_runtime`]; mirrors the aave
/// verify seam).
///
/// # Args
///
/// - `database_path` — the readable `DegenbotDb` path holding the committed map.
/// - `rpc_url` — the HTTP RPC endpoint.
/// - `chain_id` — the chain the pool's committed rows belong to.
/// - `family` — `"v3"` (a pool contract) or `"v4"` (the `PoolManager`
///   singleton).
/// - `address_or_manager` — the V3 pool address, or the V4 `PoolManager`
///   address.
/// - `pool_id` — the V4 `PoolId` (bytes32 hex, `0x…`); `None` for `"v3"`.
/// - `block_number` — the block the on-chain reads pin.
///
/// # Raises
///
/// `ValueError` for a key that names no pool (unknown family, malformed
/// address or `pool_id`, a `pool_id` on a V3 call, none on a V4 call), for a
/// DB or RPC failure, and when no ambient tokio runtime exists.
#[pyfunction]
#[pyo3(signature = (database_path, rpc_url, chain_id, family, address_or_manager, pool_id, block_number))]
#[expect(clippy::too_many_arguments)]
fn verify_liquidity_map(
    py: Python<'_>,
    database_path: &str,
    rpc_url: &str,
    chain_id: i64,
    family: &str,
    address_or_manager: &str,
    pool_id: Option<&str>,
    block_number: u64,
) -> PyResult<Vec<Py<PyDict>>> {
    use degenbot_db::{ComputedLiquidityUpdate, DegenbotDb};
    use degenbot_pool_updater::verify_liquidity_map_on_chain;
    use degenbot_rpc::liquidity_verifier::LiquidityMapTarget;

    let path = PathBuf::from(database_path);
    let identity = parse_verify_identity(family, address_or_manager, pool_id)?;

    let divergences = py
        .detach(move || {
            // The verify seams' runtime policy, spelled once in the seam
            // module: resolve the ambient runtime BEFORE any DB work, and
            // refuse a missing one with a typed error instead of building +
            // dropping a per-call multi-thread runtime (its default worker
            // count is the raw core count — the dead tokio-rt-worker churn
            // source in the live process).
            crate::provider_seam::require_ambient_runtime()?;
            let db = DegenbotDb::open(&path).map_err(RunError::Db)?.0;
            let conn = db.lock();
            let (computed, target) = match &identity {
                VerifyIdentity::V3 {
                    pool_address,
                    db_key,
                } => {
                    let state =
                        DegenbotDb::fetch_v3_pool_update_state_on_conn(&conn, chain_id, db_key)
                            .map_err(RunError::Db)?
                            .ok_or_else(|| {
                                RunError::Db(degenbot_db::DbError::Decode(format!(
                                    "v3 pool {db_key} not found on chain {chain_id}"
                                )))
                            })?;
                    let tick_data = DegenbotDb::fetch_v3_liquidity_map_on_conn(
                        &conn,
                        state.pool_id,
                        state.snapshot_block(),
                    )
                    .map_err(RunError::Db)?;
                    (
                        ComputedLiquidityUpdate {
                            pool_id: state.pool_id,
                            tick_spacing: state.tick_spacing,
                            tick_data,
                            last_event: None,
                        },
                        LiquidityMapTarget::V3(*pool_address),
                    )
                }
                VerifyIdentity::V4 {
                    pool_manager,
                    pool_id,
                    db_key,
                } => {
                    let state =
                        DegenbotDb::fetch_v4_pool_update_state_on_conn(&conn, db_key, chain_id)
                            .map_err(RunError::Db)?
                            .ok_or_else(|| {
                                RunError::Db(degenbot_db::DbError::Decode(format!(
                                    "v4 pool {db_key} not found on chain {chain_id}"
                                )))
                            })?;
                    let tick_data = DegenbotDb::fetch_v4_liquidity_map_on_conn(
                        &conn,
                        state.pool_id,
                        state.snapshot_block(),
                    )
                    .map_err(RunError::Db)?;
                    (
                        ComputedLiquidityUpdate {
                            pool_id: state.pool_id,
                            tick_spacing: state.tick_spacing,
                            tick_data,
                            last_event: None,
                        },
                        LiquidityMapTarget::V4 {
                            pool_manager: *pool_manager,
                            pool_id: *pool_id,
                        },
                    )
                }
            };
            let provider = crate::provider_seam::drive(crate::provider_seam::live_provider(
                rpc_url,
                crate::provider_seam::UPDATER_MAX_RETRIES,
            ))
            .map_err(RunError::from)?;
            crate::provider_seam::drive(verify_liquidity_map_on_chain(
                &provider,
                target,
                &computed,
                block_number,
            ))
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
/// The `degenbot._ffi.pool` Python submodule (declarative `#[pymodule]`),
/// carrying the pool-updater chunk-loop + liquidity-map verify pyfunctions.
/// The parent module registers the submodule itself and its `sys.modules`
/// entry. `CancelHandle` is registered by `crate::cancel::cancel` (shared
/// with the Aave updater seam).
#[pymodule(submodule)]
#[pyo3(module = "degenbot._ffi")]
pub mod pool {
    #[pymodule_export]
    use super::{run_pool_update, verify_liquidity_map};
}

#[cfg(all(test, feature = "auto-initialize"))]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The `PoolId` the V4 verify cases carry.
    const POOL_ID_HEX: &str = "0x0101010101010101010101010101010101010101010101010101010101010101";

    /// The V3 pool address fake the offline cases carry.
    const V3_POOL: &str = "0x0000000000000000000000000000000000000001";

    /// The V4 `PoolManager` address fake the offline cases carry.
    const V4_MANAGER: &str = "0x0000000000000000000000000000000000000002";

    /// With NO ambient tokio runtime, the verify seam must fail with the typed
    /// no-ambient-runtime error INSTEAD of building + dropping a per-call
    /// multi-thread runtime (its default worker count is the raw core count —
    /// the dead tokio-rt-worker churn seen in the live process). An empty
    /// in-memory DB + an unroutable RPC keep the test offline; the
    /// runtime-policy check fires before any DB work, for BOTH key shapes.
    #[test]
    fn verify_no_ambient_runtime_errors_instead_of_spawning() {
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "test thread must have no ambient tokio runtime"
        );
        Python::attach(|py| {
            for (family, address_or_manager, pool_id) in
                [("v3", V3_POOL, None), ("v4", V4_MANAGER, Some(POOL_ID_HEX))]
            {
                let err = verify_liquidity_map(
                    py,
                    ":memory:",
                    "http://127.0.0.1:1",
                    1,
                    family,
                    address_or_manager,
                    pool_id,
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
            }
        });
    }

    /// A key that names no pool is a `ValueError` raised before the DB open,
    /// the provider build, and the runtime-policy check.
    #[test]
    fn verify_refuses_a_key_that_names_no_pool() {
        use pyo3::exceptions::PyValueError;

        Python::attach(|py| {
            for (family, address_or_manager, pool_id) in [
                ("v2", V3_POOL, None),
                ("v3", V3_POOL, Some(POOL_ID_HEX)),
                ("v4", V4_MANAGER, None),
                ("v4", "not-an-address", Some(POOL_ID_HEX)),
                ("v4", V4_MANAGER, Some("0xnothex")),
            ] {
                let err = verify_liquidity_map(
                    py,
                    ":memory:",
                    "http://127.0.0.1:1",
                    1,
                    family,
                    address_or_manager,
                    pool_id,
                    1,
                )
                .unwrap_err();
                assert!(
                    err.is_instance_of::<PyValueError>(py),
                    "family {family:?} with pool_id {pool_id:?} must be refused as                      ValueError, got: {err}"
                );
            }
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

    /// The committed golden capture: the Uniswap V3 factory's `PoolCreated`
    /// logs plus the whole-chain V3 `Mint`/`Burn` scan over blocks
    /// `26_102_622..=26_102_626`, recorded against the live node by the
    /// recorder example (drift-gated corpus).
    const CASSETTE_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../tests/fixtures/cassettes/pool_update_chunk_26102622-26102626.json"
    );

    /// The Uniswap V3 factory the capture was recorded against.
    const V3_FACTORY: &str = "0x1F98431c8aD98523631AE4a59f267346ea31F984";

    /// Independent literal: the exact number of ledger entries the replayed
    /// chunk serves per run — one `eth_getLogs` for the factory's
    /// `PoolCreated` pass plus one whole-chain `eth_getLogs` for the V3
    /// `Mint`/`Burn` scan (the run resolves its tip from the pinned span, so
    /// no `eth_blockNumber`).
    const EXPECTED_RPC_ROUND_TRIPS: u64 = 2;

    /// A scratch database under the process temp dir, removed on drop so a
    /// failing assertion cannot leak it.
    struct ScratchDb(std::path::PathBuf);

    impl ScratchDb {
        fn new(tag: &str) -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the clock is after the epoch")
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "degenbot-provider-seam-{tag}-{}-{stamp}.db",
                std::process::id()
            )))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScratchDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            for suffix in ["-wal", "-shm"] {
                let sibling = std::path::PathBuf::from(format!("{}{suffix}", self.0.display()));
                let _ = std::fs::remove_file(sibling);
            }
        }
    }

    /// Seed one ACTIVE `uniswap_v3` exchange stamped at `from - 1`, so the
    /// run's fetch window is exactly the cassette's recorded span.
    fn seeded_db(path: &Path, chain_id: i64, from: u64) {
        use degenbot_db::DegenbotDb;

        let (db, _schema_state) = DegenbotDb::open_for_writes(path).unwrap();
        let factory: alloy::primitives::Address = V3_FACTORY.parse().unwrap();
        let exchange = db
            .upsert_exchange(chain_id, "uniswap_v3", factory, None)
            .unwrap();
        db.set_exchange_active(exchange.id, true).unwrap();
        db.set_exchange_last_update_block(chain_id, exchange.id, i64::try_from(from - 1).unwrap())
            .unwrap();
    }

    /// Read + parse the committed corpus, checking the drift gate on the way.
    fn committed_cassette() -> (Vec<u8>, degenbot_rpc::cassette::Cassette) {
        use degenbot_rpc::cassette::{verify_cassette_bytes, Cassette};

        let bytes = std::fs::read(CASSETTE_PATH).expect("the committed cassette must exist");
        verify_cassette_bytes(&bytes).expect("the committed cassette passes its drift gate");
        let cassette = Cassette::from_json_bytes(&bytes).expect("a valid cassette");
        (bytes, cassette)
    }

    /// A `CassetteReplayTransport`-backed provider drives the SAME entry the
    /// operator's `rpc_url` call drives (ADR-068 D5: the provider seam is
    /// injection). No socket exists to dial, so the replayed run's real
    /// fetch → decode → apply → stamp cycle reads only the recorded ledger.
    #[test]
    fn run_pool_update_entry_accepts_a_cassette_backed_provider() {
        use degenbot_rpc::cassette_replay::CassetteReplayTransport;
        use std::sync::atomic::AtomicBool;

        let (_bytes, cassette) = committed_cassette();
        let chain_id = i64::try_from(cassette.chain_id).unwrap();
        let span = cassette.provenance.span;
        assert_eq!(span.from_block, 26_102_622);
        assert_eq!(span.to_block, 26_102_626);

        let transport = CassetteReplayTransport::new(cassette);
        let provider = transport.as_alloy_provider();

        let scratch = ScratchDb::new("replay");
        seeded_db(scratch.path(), chain_id, span.from_block);

        let report = run_pool_update_injected(
            scratch.path(),
            chain_id,
            Some(span.to_block),
            span.to_block - span.from_block + 1,
            10_000,
            provider,
            Arc::new(AtomicBool::new(false)),
            false,
            None,
            false,
        )
        .expect("the replayed run must commit cleanly");

        assert_eq!(
            report.from_block, span.from_block,
            "the run starts at the pin"
        );
        assert_eq!(
            report.to_block, span.to_block,
            "the run advances to the pin"
        );
        assert_eq!(report.chunks_committed, 1, "the whole span is one chunk");
        assert_eq!(
            report.total_pools_written, 1,
            "a 0 here would mean decode/apply never ran over the cassette"
        );

        let served = transport.served_snapshot();
        assert_eq!(
            served.requests, served.served,
            "every request must be a served ledger entry — a fixture gap is loud, not silent"
        );
        assert_eq!(
            served.served, EXPECTED_RPC_ROUND_TRIPS,
            "the replayed chunk's fetch surface drifted"
        );
    }

    /// The red half of the same seam pin: one ledger key removed in memory —
    /// the entry must refuse with the typed provider error rather than
    /// silently miss a fetch. The committed corpus file is never written, so
    /// it stays byte-identical (asserted through its keccak digest), and the
    /// refused run leaves the stamp where it started.
    #[test]
    fn run_pool_update_entry_refuses_a_cassette_ledger_gap() {
        use degenbot_db::DegenbotDb;
        use degenbot_rpc::cassette_replay::CassetteReplayTransport;
        use std::sync::atomic::AtomicBool;

        let (bytes, mut cassette) = committed_cassette();
        let before = alloy::primitives::keccak256(&bytes);
        let chain_id = i64::try_from(cassette.chain_id).unwrap();
        let span = cassette.provenance.span;

        let missing = cassette
            .entries
            .keys()
            .find(|key| key.contains("eth_getLogs"))
            .expect("the corpus carries the chunk's getLogs entries")
            .clone();
        cassette.entries.remove(&missing);

        let transport = CassetteReplayTransport::new(cassette);
        let provider = transport.as_alloy_provider();

        let scratch = ScratchDb::new("ledger-gap");
        seeded_db(scratch.path(), chain_id, span.from_block);

        let err = run_pool_update_injected(
            scratch.path(),
            chain_id,
            Some(span.to_block),
            span.to_block - span.from_block + 1,
            10_000,
            provider,
            Arc::new(AtomicBool::new(false)),
            false,
            None,
            false,
        )
        .expect_err("a ledger gap must refuse the run");
        assert!(
            matches!(err, RunError::Provider(_)),
            "expected the typed provider refusal, got {err:?}"
        );

        // The corpus on disk was never touched: the mutation lived in memory.
        let after =
            alloy::primitives::keccak256(std::fs::read(CASSETTE_PATH).expect("cassette read"));
        assert_eq!(after, before, "the committed cassette must not be mutated");

        // Nothing was written and the restart cursor did not move.
        let (db, _schema_state) = DegenbotDb::open(scratch.path()).unwrap();
        let conn = db.lock();
        let pools: i64 = conn
            .query_row("SELECT COUNT(*) FROM pools", [], |row| row.get(0))
            .unwrap();
        assert_eq!(pools, 0, "the refused chunk must leave no pool rows");
        let stamp: Option<i64> = conn
            .query_row(
                "SELECT MAX(last_update_block) FROM exchanges WHERE active = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            stamp,
            Some(i64::try_from(span.from_block - 1).unwrap()),
            "the restart cursor must not advance past the pin"
        );
    }
}
