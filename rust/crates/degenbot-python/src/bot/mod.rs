//! Per-domain binding-crate modules.
//!
//! `engine/` is the first inhabitant; step 6 of the binding-layer reorg
//! relocates the other `bot` / `bot::pool` wrappers alongside.
//! (ergo UG6FKN task WXHGOH.)

pub mod deployments;
pub mod dex_identity;
pub mod engine;
pub mod pool;
pub mod pump;
pub mod py_bot_io;
pub mod subscriber;
#[cfg(feature = "auto-initialize")]
pub mod test_gil;
pub mod token;

// === PyBot (moved from the former root `py_bot.rs`) ===
//
// `PyO3` wrappers for `BotState` — thin Python handle over Rust-owned state.
// Implements the Polars-inspired three-layer topology (ADR-005): Rust [`BotState`]
// core → `PyBot` `#[pyclass]` wrapper holding `Arc<parking_lot::RwLock<BotState>>`
// → Python `BotState` session. `PyLiquidityPool`/`PyErc20Token` clone the same
// `Arc` so many Python handles reference one Rust-owned `BotState`.
// See: `docs/adr/ADR-005-polars-inspired-three-layer-architecture.md`.

use crate::prelude::*;
use std::sync::Arc;

use alloy::primitives::Address;

use crate::bot::engine::{
    hex_string_to_pool_id, map_builder_err, map_register_v2_err, map_register_v3_err,
    map_register_v4_err, SpecViolationError,
};

/// Narrow a Python-supplied `U256` reserve to `U112` (the on-chain `uint112`
/// width), raising `SpecViolationError` if bits ≥ 112 are set.
fn narrow_reserve(
    value: alloy::primitives::U256,
    field: &'static str,
) -> PyResult<alloy::primitives::aliases::U112> {
    degenbot_pools::spec_bounds::narrow_v2_reserve(value, field)
        .map_err(|sv| SpecViolationError::new_err(format!("{sv}")))
}
use crate::bot::pool::PyLiquidityPool;
use crate::bot::token::PyErc20Token;
use degenbot_bot::bot_core::PoolTickCoverage;
use degenbot_bot::bot_core::{
    Bot, BotState, RegisterAerodromeV2PoolParams, RegisterBalancerStablePoolParams,
    RegisterBalancerWeightedPoolParams, RegisterCurvePoolParams, RegisterV2PoolParams,
    RegisterV3PoolParams, RegisterV4PoolParams, SimulateSwapError, V4PoolKey,
};
use degenbot_pools::state_history::JournalError;
use degenbot_uniswap::dex_identity::DexVariant;
use pyo3::types::{PyDict, PyList};
use pyo3::Bound;

/// Build an `alloy::rpc::types::Log` from the WS-log shape Python tests pass —
/// `(address, topics, data, block_number)` reconstructed into the same
/// `alloy::rpc::types::Log` the `BlockPump` feeds `Bot::dispatch_log`. Hex
/// strings accept an optional `0x` prefix. This is the marshalling seam for
/// the Python-facing `dispatch_log` (ADR-006, deferred §17 closure): it lets
/// an offline test drive the full pump→dispatch→notify→solve loop without a
/// live WS node, reusing the existing pure-logic dispatcher untouched.
fn build_rpc_log(
    address: &str,
    topics: Vec<String>,
    data: &str,
    block_number: u64,
) -> PyResult<alloy::rpc::types::Log> {
    let addr: Address = address.parse().map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("Invalid address '{address}': {e}"))
    })?;
    let mut topic_hashes = Vec::with_capacity(topics.len());
    for t in topics {
        let stripped = t.strip_prefix("0x").unwrap_or(&t);
        let b: alloy::primitives::B256 = stripped.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid topic '{t}': {e}"))
        })?;
        topic_hashes.push(b);
    }
    let data_stripped = data.strip_prefix("0x").unwrap_or(data);
    let data_bytes = alloy::hex::decode(data_stripped).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("Invalid data hex '{data}': {e}"))
    })?;
    let inner = alloy::primitives::Log::new_unchecked(
        addr,
        topic_hashes,
        alloy::primitives::Bytes::from(data_bytes),
    );
    Ok(alloy::rpc::types::Log {
        inner,
        block_hash: None,
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    })
}

/// Encode a byte slice as a lowercase hex string (no "0x" prefix).
fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX_CHARS[(b >> 4) as usize] as char);
        s.push(HEX_CHARS[(b & 0x0f) as usize] as char);
    }
    s
}

// ---------------------------------------------------------------------------
// PyBot — owns the Bot orchestrator; hands out shared BotState Arcs
// ---------------------------------------------------------------------------

/// Python handle to the per-chain `Bot` orchestrator (ADR-006 D4).
///
/// Python constructs a `PyBot` (or receives a shared handle), registers
/// pools/tokens, then reads results. `PyBot` owns a [`Bot`] via `Arc` and hands
/// out clones of its shared `Arc<RwLock<BotState>>` (`state_arc`) so
/// `PyLiquidityPool` / `PyErc20Token` / `ArbitrageEngine` all reach ONE
/// Rust-owned `BotState`; `BlockPump` clones the same `Arc<Bot>` so its
/// `dispatch_log` writes flow through to the engine's reads (N handles → one
/// state — the Polars three-layer invariant, preserved + generalized by D4).
#[pyclass(name = "Bot", skip_from_py_object, module = "degenbot._ffi")]
pub struct PyBot {
    bot: Arc<Bot>,
    /// ADR-006 D4 (T3): the pump lifecycle state, shared with the
    /// `PyArbitrageEngine` this bot owns. `None` until an engine is
    /// constructed against this bot (the engine attaches its `Arc<PumpState>`
    /// back here during `new()`). Once attached, the three pump methods —
    /// `subscribe`, `backfill_from_snapshot`, `resume` — are drivable from
    /// `PyBot` (the D4 owner) and read/write the SAME `PumpState` the
    /// engine's snapshot/solve slices read.
    pump: parking_lot::Mutex<Option<Arc<crate::bot::pump::PumpState>>>,
    /// Cached read-only `SnapshotDb` handle armed at `load_snapshot_from_db`
    /// time (Decisions 5 (B) + 9 (A); epic `XEANMB`). `None` for cold-start
    /// (no DB) or before the snapshot load is attempted. The registration-path
    /// `PyO3` functions clone this Arc to feed `&dyn TickMapDb` to the tick-map
    /// assembly helper's Db arm (`bot_core::tick_assembly`).
    ///
    /// One connection, opened by `load_snapshot_from_db` with a held deferred
    /// read transaction + **retained**. The snapshot load + the per-pool Db
    /// probe share the SAME `Mutex<Connection>` + its open tx — every read
    /// shares one frozen DB snapshot across `build_paths` (WAL MVCC).
    /// `close_snapshot_tx()` commits the tx at end of `build_paths`.
    db: parking_lot::Mutex<Option<Arc<degenbot_db::snapshot_db::SnapshotDb>>>,
}

/// Crate-internal Rust surface on `PyBot` (not Python-visible).
impl PyBot {
    /// Hand out a clone of the shared `Arc<Bot>` so `BlockPump` (and other
    /// sibling consumers) drive the SAME `Bot`'s `dispatch_log` + state that
    /// `PyBot` owns (ADR-006 D4: pump lifecycle relocation onto `Bot`).
    #[must_use]
    pub(crate) fn bot_arc(&self) -> Arc<Bot> {
        Arc::clone(&self.bot)
    }

    /// Sanctioned `BotState` read access for pymethod code (GIL/`BotState`
    /// inversion class, incidents 2026-08-20/21). The guard is acquired
    /// INSIDE `py.detach`, so the GIL is never held while parked on the lock;
    /// pyo3's `Ungil` bound (= `Send` on stable) keeps GIL-bound values out
    /// of the closure, and the `!Send` guard can never escape it. Direct
    /// `state_arc().read()/.write()` in pymethod bodies is forbidden — the
    /// source scan (`tests/gil_state_write_concurrency.rs`) permits it only
    /// inside these accessors and the marked pure-Rust test seams.
    pub(crate) fn with_state<T>(&self, py: Python<'_>, f: impl FnOnce(&BotState) -> T + Send) -> T
    where
        T: Send,
    {
        py.detach(|| {
            let core = self.bot.state_arc();
            // T1-scan-exempt: sanctioned accessor — guard inside py.detach by definition.
            let guard = core.read();
            f(&guard)
        })
    }

    /// Sanctioned `BotState` write access — see [`Self::with_state`].
    pub(crate) fn with_state_mut<T>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&mut BotState) -> T + Send,
    ) -> T
    where
        T: Send,
    {
        py.detach(move || {
            let core = self.bot.state_arc();
            // T1-scan-exempt: sanctioned accessor — guard inside py.detach by definition.
            let mut guard = core.write();
            f(&mut guard)
        })
    }

    /// The cached read-only `SnapshotDb` handle armed by
    /// `load_snapshot_from_db` (Decision 5 (B) / task A6J5HG + epic `XEANMB`),
    /// or `None` on the cold-start path (no DB configured, or the snapshot
    /// load hasn't run, or `close_snapshot_tx()` dropped it). The registration-
    /// path `PyO3` functions (`assemble_v3_tick_map` / `assemble_v4_tick_map`)
    /// clone this Arc and pass `&*arc as &dyn TickMapDb` to
    /// `bot_core::tick_assembly::assemble_*`'s `db` param. Returns a fresh
    /// `Arc` clone — the caller owns the lifetime of the borrow; `PyBot`
    /// retains its own Arc so subsequent callers still get `Some`.
    #[must_use]
    pub(crate) fn db_handle(&self) -> Option<Arc<degenbot_db::snapshot_db::SnapshotDb>> {
        self.db.lock().clone()
    }

    /// ADR-006 D4 (T3): attach the pump lifecycle state owned by a
    /// `PyArbitrageEngine` constructed against this bot. Called from
    /// `PyArbitrageEngine::new` when `py_bot` is supplied. After this, the
    /// pump methods on `PyBot` drive the same `PumpState` the engine reads.
    pub(crate) fn attach_pump_state(&self, pump: Arc<crate::bot::pump::PumpState>) {
        *self.pump.lock() = Some(pump);
    }

    /// Borrow the attached `PumpState`, or error if no engine was constructed
    /// against this bot.
    // backfill_from_snapshot/resume) in the #[pymethods] impl
    fn pump_state(&self) -> PyResult<Arc<crate::bot::pump::PumpState>> {
        self.pump.lock().clone().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "No engine attached to this Bot. Construct a ArbitrageEngine(py_bot=...) \
                 before calling pump lifecycle methods on Bot.",
            )
        })
    }
}

#[pymethods]
impl PyBot {
    #[new]
    #[pyo3(signature = (chain_id = 0))]
    #[must_use]
    pub fn new(chain_id: u64) -> Self {
        // ADR-006 slice 8b: the Python ``Bot`` facade is now single-chain,
        // so it passes its ``config.default_chain_id`` here. The ``chain_id = 0``
        // default keeps the bare ``PyBot()`` lower-level test fixtures (which
        // only exercise the Rust core) working without a chain invariant.
        // `Arc<Bot>` so `BlockPump` clones the same orchestrator (ADR-006 D4).
        Self {
            bot: Arc::new(Bot::new(chain_id)),
            pump: parking_lot::Mutex::new(None),
            db: parking_lot::Mutex::new(None),
        }
    }

    /// Load the V3 + V4 DB snapshot into the core `BotState` (B3OROH, JUCFCB).
    ///
    /// Called at Python `Bot.__init__` time when a DB path is configured
    /// (Shape 2: eager construction-time load). Opens a read-only
    /// `DegenbotDb` handle from `db_path`, then calls the core
    /// `Bot::load_snapshot_from_db` — the single Rust entry point that
    /// `stream_liquidity_maps`-loads V3+V4 pools into the core `SnapshotStore`s
    /// + records `S = min(fetch_newest_update_block(V3), V4)`. After this,
    /// pool registration consumes the store via `take()` (RUQ637) — the
    /// Python builder passes `tick_data=None, coverage=Tracked` and lets the
    /// store decide Tracked-vs-Sparse per pool.
    ///
    /// `None`/cold-start (no pools) is NOT an error — the pump anchors on
    /// `first_observed_block` at resume. Idempotent across the two families,
    /// but a second call after a successful load will `begin_load` a store
    /// that's already loaded (panics) — call exactly once at construction.
    ///
    /// # Errors
    /// `PyRuntimeError` on a DB open/read failure or a liquidity value
    /// out of range.
    #[pyo3(signature = (db_path, chain_id))]
    fn load_snapshot_from_db(&self, db_path: &str, chain_id: u64) -> PyResult<()> {
        // Epic XEANMB: open a `SnapshotDb` — a read-only handle with a held
        // deferred read transaction. `Bot::load_snapshot_from_db` reads `S =
        // min(fetch_newest_update_block(V3), V4)` INSIDE the held tx so `S`
        // and every per-pool `fetch_liquidity_map` read share one frozen DB
        // snapshot across `build_paths` (the consistency replacement for the
        // retired `SnapshotStore`). `PyBot.close_snapshot_tx()` commits the
        // tx at end of `build_paths` to release the WAL snapshot.
        let snap = degenbot_db::snapshot_db::SnapshotDb::open(&std::path::PathBuf::from(db_path))
            .map_err(|e| crate::db::db_err_to_py(&e))?
            .0;
        self.bot
            .load_snapshot_from_db(&snap, chain_id)
            .map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "load_snapshot_from_db failed: {e}"
                ))
            })?;
        // Retain the `SnapshotDb` as a long-lived `Arc` so the registration-
        // path PyO3 functions (`assemble_v3_tick_map` etc.) clone it to feed
        // `&dyn TickMapDb` to the tick-map assembly helper's Db arm. Stored
        // AFTER the snapshot load succeeded, so a failure leaves `db` as
        // `None` (cold-start).
        *self.db.lock() = Some(Arc::new(snap));
        Ok(())
    }

    /// Commit + drop the held snapshot read transaction (epic `XEANMB`).
    /// Call after `build_paths` finishes so the WAL snapshot is released + the
    /// updater's checkpoint can reclaim `-wal` space. After this, `db_handle()`
    /// returns `None` (the `assemble_*` Db arm goes through the held tx — once
    /// it's closed, there's no Db arm for late registrations; they'd need a
    /// fresh handle). Idempotent on a not-loaded bot (no-op).
    ///
    /// # Errors
    /// `PyRuntimeError` if the `COMMIT` fails.
    fn close_snapshot_tx(&self, py: Python<'_>) -> PyResult<()> {
        // Canary (epic XEANMB task 5.7): capture S_snapshot (read inside the
        // held tx) before committing, then re-read S_live after COMMIT on the
        // same connection (now seeing the live DB). If S_live > S_snapshot the
        // pool_updater committed concurrently with startup — a discipline
        // violation. Correctness was already preserved by the held tx; the
        // canary only surfaces it.
        // GIL hygiene: read guard acquired inside py.detach (inversion class).
        let s_snapshot = self.with_state(py, degenbot_bot::bot_core::BotState::snapshot_seed_block);
        let chain_id = self.bot.chain_id();
        let snap = self.db.lock().take();
        if let Some(snap) = snap {
            // `Arc::try_unwrap` succeeds only if `assemble_*` calls released
            // their clones. During `build_paths` the Db arm clones per-call +
            // drops before `close_snapshot_tx` runs, so at this point the only
            // remaining `Arc` is this one. A failure (clones remain) surfaces
            // as a `RuntimeError` rather than silently leaking the tx.
            match std::sync::Arc::try_unwrap(snap) {
                Ok(snap) => {
                    let chain = i64::try_from(chain_id).unwrap_or(0);
                    let report = snap
                        .close_with_canary(s_snapshot, chain)
                        .map_err(|e| crate::db::db_err_to_py(&e))?;
                    if report.advanced {
                        tracing::warn!(
                            s_snapshot = report.s_snapshot.unwrap_or(0),
                            s_live = report.s_live.unwrap_or(0),
                            "DB advanced during startup — operator discipline violation"
                        );
                    }
                }
                Err(_) => {
                    return Err(pyo3::exceptions::PyRuntimeError::new_err(
                        "close_snapshot_tx: SnapshotDb Arc still held (clone leak — \
                         a caller didn't drop its handle)",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Attach a construction-I/O handle built from a Python provider + an
    /// optional `database_path` (architecture review 2025-07-18 / candidate 1).
    /// The Python `Bot.__init__` calls this after constructing `PyBot` so the
    /// 7 generic RPC + 12 DB atomic methods on `PyBotIo` can delegate through
    /// the core trait objects. Alloy-only — a non-alloy provider raises
    /// `RuntimeError` (Q6-narrow; the 27 choreography wrappers keep their
    /// Python-delegation fallback this slice, deleted with the builder-
    /// choreography port).
    ///
    /// `database_path = None` → the DB half is `NoDb` (every read returns
    /// `None`/empty, the no-DB path). `Some(path)` → a held `DegenbotDb` is
    /// opened (WAL permits the concurrent connection) + wrapped in
    /// `DegenbotDbConstruction`; construction DB reads route through this held
    /// connection (no per-call `DegenbotDb::open`).
    #[pyo3(signature = (provider, database_path=None))]
    #[expect(clippy::unnecessary_wraps, clippy::needless_pass_by_value)]
    fn attach_construction_io(
        &self,
        py: Python<'_>,
        provider: Bound<'_, PyAny>,
        database_path: Option<&str>,
    ) -> PyResult<()> {
        use crate::bot::py_bot_io::extract_native_alloy;
        use degenbot_bot::bot_core::construction_io::{
            AlloyRpcConstruction, ConstructionIo, DegenbotDbConstruction, NoDb,
        };

        // Q6-narrow: the trait is alloy-only. A non-alloy provider (legacy
        // test doubles that exercise the choreography fallback) yields `None`
        // here — we skip attaching, leaving `construction_io = None`. The 7
        // RPC + 12 DB methods on `PyBotIo` then fall back to the `self.alloy` /
        // Python path (the `None` branch in each). This preserves the
        // choreography-test path; a production driver MUST supply an alloy
        // provider to get the trait delegation (the migration note documents
        // this; the fallback is deleted with the builder-choreography port).
        let Some(alloy) = extract_native_alloy(&provider) else {
            return Ok(());
        };
        let rpc = std::sync::Arc::new(AlloyRpcConstruction::new((*alloy).clone()));

        let db: std::sync::Arc<
            dyn degenbot_bot::bot_core::construction_io::DbConstruction + Send + Sync,
        > = match database_path {
            Some(path) => {
                let path_buf = std::path::PathBuf::from(path);
                // `open_for_writes` — the construction executor does reads
                // AND the `update_erc20_token_metadata` write-back, so the
                // held connection must be write-capable. A missing file
                // (SQLAlchemy creates the DB lazily on first write) → fall
                // back to `NoDb` so the DB methods return the no-DB shape
                // (matching the original `database_path` cold-start skip);
                // a `Bot` restart after the file exists picks it up.
                match py.detach(|| degenbot_db::DegenbotDb::open_for_writes(&path_buf)) {
                    Ok((db, _state)) => std::sync::Arc::new(DegenbotDbConstruction::new(db)),
                    Err(e) => {
                        tracing::debug!(
                            path = %path,
                            %e,
                            "Construction-I/O DB open failed; falling back to NoDb"
                        );
                        std::sync::Arc::new(NoDb)
                    }
                }
            }
            None => std::sync::Arc::new(NoDb),
        };

        let io = ConstructionIo::new(db, rpc);
        // Attach to the shared `Bot` (interior-mutable `construction_io` field);
        // `Bot` is held as `Arc` by `PyBot`, so we mutate through the `RwLock`
        // rather than adopting the `Arc` out.
        self.bot.set_construction_io(io);
        Ok(())
    }

    /// The snapshot seed block `S` (or `None` when no DB snapshot was loaded —
    /// the cold-start path). Python reads this in `engine_registry.start()`
    /// to stash `_verify_snapshot_block` for the per-pool two-step verify.
    #[getter]
    fn snapshot_seed_block(&self, py: Python<'_>) -> Option<u64> {
        // GIL hygiene: read guard acquired inside py.detach (inversion class).
        self.with_state(py, degenbot_bot::bot_core::BotState::snapshot_seed_block)
    }

    /// Subscribe to the WS `newHeads` + logs streams (ADR-006 D4 T3).
    ///
    /// The Bot-owned pump entry point — delegates to the shared `PumpState`
    /// attached when a `ArbitrageEngine` was constructed against this bot.
    /// Blocks (sync, via the shared tokio runtime) until the first block is
    /// observed, then returns the first WS block number (the backfill target).
    ///
    /// # Errors
    /// `PyRuntimeError` if no engine is attached, the pump is already
    /// started/subscribed, or the WS subscribe fails.
    #[pyo3(signature = (rpc_url))]
    fn subscribe(&self, py: Python<'_>, rpc_url: &str) -> PyResult<u64> {
        self.pump_state()?.subscribe(py, rpc_url)
    }

    /// Resume the pump — begin normal WS processing (ADR-006 D4 T3).
    ///
    /// The snapshot→WS gap is closed automatically inside the core
    /// `BlockPump::resume_from_subscribe` (J3FMDO); the pyo3
    /// `backfill_from_snapshot` method is retired (2SM4Y7). Delegates to the
    /// shared `PumpState`.
    fn resume(&self, py: Python<'_>) -> PyResult<()> {
        self.pump_state()?.resume(py)
    }

    /// Stop the pump and signal the Rust core to clean up (ADR-006 D4).
    ///
    /// The symmetric teardown half of the `subscribe` → `backfill_from_snapshot`
    /// → `resume` lifecycle. Sets the shutdown flag and aborts the spawned
    /// pump task so a Ctrl-C exits promptly (the pump loop otherwise blocks
    /// up to 60s on a silent WS stream). Idempotent — safe to call from both
    /// the `__aexit__` path and a signal handler. Delegates to the shared
    /// `PumpState`.
    fn stop(&self, _py: Python<'_>) -> PyResult<()> {
        self.pump_state()?.stop()
    }

    /// Set the HTTP RPC URL used for verification (ADR-006 D4 T4).
    /// Delegates to the shared `PumpState`.
    #[pyo3(signature = (rpc_url))]
    fn set_verify_rpc_url(&self, rpc_url: &str) {
        if let Ok(pump) = self.pump_state() {
            pump.set_verify_rpc_url(rpc_url);
        }
    }

    /// Set the `StateView` contract address for V4 verification (ADR-006 D4 T4).
    #[pyo3(signature = (state_view_address))]
    fn set_verify_state_view(&self, state_view_address: &str) {
        if let Ok(pump) = self.pump_state() {
            pump.set_verify_state_view(state_view_address);
        }
    }

    /// Run a single V3 pool's registration verify-lifecycle end-to-end
    /// (ADR-022 D1): the core-owned `quarantine → seed-verify → drain+pin →
    /// post-drain-verify → set_live` choreography that replaces the Python
    /// driver's separate step calls. **Sparse** → immediate no-op (`Live`, no
    /// RPC); **Tracked** → verified with the mismatch tripwire before `Live`.
    /// Uses the bot's single verify provider + stored verify config (D-B/D-C).
    #[pyo3(signature = (address, snapshot_block))]
    fn run_v3_registration_lifecycle<'py>(
        &self,
        py: Python<'py>,
        address: String,
        snapshot_block: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.pump_state()?
            .run_v3_registration_lifecycle(py, address, snapshot_block)
    }

    /// V4 twin of `run_v3_registration_lifecycle`, keyed by
    /// (`pool_manager_address`, `pool_id_hex`). A tracked V4 pool with no
    /// `verify_state_view` configured fails fast (D-C no-config posture).
    #[pyo3(signature = (pool_manager_address, pool_id_hex, snapshot_block))]
    fn run_v4_registration_lifecycle<'py>(
        &self,
        py: Python<'py>,
        pool_manager_address: String,
        pool_id_hex: String,
        snapshot_block: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.pump_state()?.run_v4_registration_lifecycle(
            py,
            pool_manager_address,
            pool_id_hex,
            snapshot_block,
        )
    }

    /// Register a V2 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    #[expect(clippy::too_many_arguments)]
    #[pyo3(signature = (address, token0, token1, reserve0, reserve1, gamma_numer0, fee_denom0, gamma_numer1, fee_denom1, factory, update_block=0, variant="uniswap-v2", stable_swap=false, fee_denominator=None))]
    fn register_v2_pool(
        &self,
        py: Python<'_>,
        address: &str,
        token0: &str,
        token1: &str,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        gamma_numer0: u64,
        fee_denom0: u64,
        gamma_numer1: u64,
        fee_denom1: u64,
        factory: &str,
        update_block: u64,
        variant: &str,
        stable_swap: bool,
        fee_denominator: Option<u64>,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let t0 = parse_address(token0)?;
        let t1 = parse_address(token1)?;
        let fac = parse_address(factory)?;
        let r0 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve0)?,
            "reserve0",
        )?;
        let r1 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve1)?,
            "reserve1",
        )?;
        let variant_enum = DexVariant::from_kebab(variant).ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err(format!("unknown variant: {variant}"))
        })?;

        // Verify the pool address against the JSON-sourced CREATE2 deployer +
        // init hash (Fork A, JC6OFG). Skipped if (chain, factory) is not in the
        // shipped JSON — preserves the manual/ad-hoc registration path.
        crate::bot::deployments::verify_v2(self.bot.chain_id(), fac, addr, t0, t1)?;

        // Resolve the JSON-sourced CREATE2 deployer + init hash (Fork A,
        // NSAZ4X). Stored on the V2 identity so the `dex` getter merges the
        // per-(chain,factory) deployer/init_hash into the protocol preset
        // (replacing the canonical-mainnet preset values). Non-JSON pools
        // default to factory-as-deployer + the V2 mainnet fallback init hash.
        let chain_id = self.bot.chain_id();
        let deployer = degenbot_uniswap::deployments::resolve_deployer(chain_id, fac);
        let init_hash_b256 = degenbot_uniswap::deployments::resolve_v2_init_hash(chain_id, fac);

        let p = RegisterV2PoolParams {
            address: addr,
            token0: t0,
            token1: t1,
            reserve0: r0,
            reserve1: r1,
            fee_token0: (gamma_numer0, fee_denom0),
            fee_token1: (gamma_numer1, fee_denom1),
            factory: fac,
            deployer,
            init_hash: init_hash_b256,
            update_block,
            variant: variant_enum,
            stable_swap,
            fee_denominator,
        };
        // Incident 2026-08-20 #2: never hold the GIL while parked on the
        // BotState write - the dispatch fan-out's per-candidate tasks hold
        // the read end across provider fetches, and a parked GIL-writer
        // freezes every GIL consumer (main asyncio, log drainer, gil-probe).
        // Evidence: /tmp/degenbot-gil-deadlock-2026-08-20 (26 readers, state 0x1b).
        self.with_state_mut(py, |s| s.register_v2_pool(&p))
            .map_err(map_register_v2_err)
    }

    /// Build + register a V2 pool through the Rust `PoolBuilder` (T4 / 4GQWZ4
    /// delegation adapter). Runs the core async builder — ALL the io
    /// choreography in Rust: immutable data, reserves, DEX-resolution incl.
    /// the Camelot branch, CREATE2 verify, deployer/init-hash, narrow reserves
    /// — on the attached `ConstructionIo`, registers into `BotState`, and
    /// returns the auto-assigned pool id for the Python caller to wrap.
    ///
    /// The `PoolBuilder` is alloy-only (Q6-narrow `ConstructionIo`): this
    /// requires an attached construction-I/O (D-B single `AlloyProvider`).
    ///
    /// # Errors
    ///
    /// `RuntimeError` if no `ConstructionIo` is attached, on a builder RPC /
    /// CREATE2 / spec / DB failure, or on a registration (already-registered /
    /// spec) failure.
    #[pyo3(signature = (address, block=None))]
    fn build_v2_pool(
        &self,
        py: Python<'_>,
        address: &str,
        block: Option<u64>,
    ) -> PyResult<(u64, String, String, String, String)> {
        use degenbot_bot::bot_core::pool_builder::builder;
        use degenbot_core::runtime::get_runtime;
        let addr = parse_address(address)?;
        let io = self.bot.construction_io_arc().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "build_v2_pool: no ConstructionIo attached (requires an alloy provider)",
            )
        })?;
        let chain_id = self.bot.chain_id();
        let params = py
            .detach(|| get_runtime().block_on(builder::build_v2(chain_id, addr, &io, block)))
            .map_err(map_builder_err)?;
        // TF7RZB-S1 (builder return surface): return the core-computed identity
        // alongside the pool_id so a facade-free registration driver can consume
        // token0/token1/address/family from the Rust builder instead of
        // re-deriving them from the pool handle. `variant` is the `DexVariant`
        // the builder resolved (kebab-case, e.g. "uniswap-v2").
        let identity = (
            params.token0.to_checksum(None),
            params.token1.to_checksum(None),
            params.address.to_checksum(None),
            params.variant.as_str().to_string(),
        );
        // Incident 2026-08-20 #2: never hold the GIL while parked on the
        // BotState write - the dispatch fan-out's per-candidate tasks hold
        // the read end across provider fetches, and a parked GIL-writer
        // freezes every GIL consumer (main asyncio, log drainer, gil-probe).
        // Evidence: /tmp/degenbot-gil-deadlock-2026-08-20 (26 readers, state 0x1b).
        let pool_id = self
            .with_state_mut(py, |s| s.register_v2_pool(&params))
            .map_err(map_register_v2_err)?;
        Ok((pool_id, identity.0, identity.1, identity.2, identity.3))
    }

    /// Build + register an Aerodrome V2 pool through the Rust `PoolBuilder`
    /// (SSSXG6 / SSD2XI delegation adapter) — the Aerodrome twin of
    /// [`Self::build_v2_pool`]. The core `builder::build_aerodrome_v2` runs the
    /// full `stable()`+`getFee()` + reserves + CREATE2 choreography and returns
    /// a [`RegisterAerodromeV2PoolParams`]; this adapter registers it into
    /// [`PoolEntry::AerodromeV2`]. Returns the `pool_id`.
    fn build_aerodrome_v2_pool(
        &self,
        py: Python<'_>,
        address: &str,
        block: Option<u64>,
    ) -> PyResult<u64> {
        use degenbot_bot::bot_core::pool_builder::builder;
        use degenbot_core::runtime::get_runtime;
        let addr = parse_address(address)?;
        let io = self.bot.construction_io_arc().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "build_aerodrome_v2_pool: no ConstructionIo attached (requires an alloy provider)",
            )
        })?;
        let chain_id = self.bot.chain_id();
        let params = py
            .detach(|| {
                get_runtime().block_on(builder::build_aerodrome_v2(chain_id, addr, &io, block))
            })
            .map_err(map_builder_err)?;
        // Incident 2026-08-20 #2: never hold the GIL while parked on the
        // BotState write - the dispatch fan-out's per-candidate tasks hold
        // the read end across provider fetches, and a parked GIL-writer
        // freezes every GIL consumer (main asyncio, log drainer, gil-probe).
        // Evidence: /tmp/degenbot-gil-deadlock-2026-08-20 (26 readers, state 0x1b).
        Ok(self.with_state_mut(py, |s| s.register_aerodrome_pool(&params)))
    }

    /// Build + register a Balancer V2 **weighted** pool through the Rust
    /// `PoolBuilder` (SSSXG6 delegation adapter). The core
    /// `builder::build_balancer_weighted` runs the full `getPoolId` + Vault
    /// `getPoolTokens` + `getSwapFeePercentage` + `getNormalizedWeights` +
    /// bytecode `PowVersion` detect + `decimals()` scaling-factor choreography
    /// and returns a [`RegisterBalancerWeightedPoolParams`]; this adapter
    /// registers it into [`PoolEntry::BalancerWeighted`]. Returns the `pool_id`.
    fn build_balancer_weighted_pool(
        &self,
        py: Python<'_>,
        address: &str,
        vault: &str,
        block: Option<u64>,
    ) -> PyResult<u64> {
        use degenbot_bot::bot_core::pool_builder::builder;
        use degenbot_core::runtime::get_runtime;
        let addr = parse_address(address)?;
        let vault_addr = parse_address(vault)?;
        let io = self.bot.construction_io_arc().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "build_balancer_weighted_pool: no ConstructionIo attached (requires an alloy provider)",
            )
        })?;
        let params = py
            .detach(|| {
                get_runtime().block_on(builder::build_balancer_weighted(
                    vault_addr, addr, &io, block,
                ))
            })
            .map_err(map_builder_err)?;
        // Incident 2026-08-20 #2: never hold the GIL while parked on the
        // BotState write - the dispatch fan-out's per-candidate tasks hold
        // the read end across provider fetches, and a parked GIL-writer
        // freezes every GIL consumer (main asyncio, log drainer, gil-probe).
        // Evidence: /tmp/degenbot-gil-deadlock-2026-08-20 (26 readers, state 0x1b).
        Ok(self.with_state_mut(py, |s| s.register_balancer_weighted_pool(&params)))
    }

    /// Build + register a Balancer V2 **stable** pool through the Rust
    /// `PoolBuilder` (SSSXG6 delegation adapter). The core
    /// `builder::build_balancer_stable` runs the `getPoolId` + Vault
    /// `getPoolTokens` + `getSwapFeePercentage` + `getAmplificationParameter`
    /// + BPT-detect + rate-provider/rate + scaling-factor + `invariant_version`
    /// resolution choreography and returns a [`RegisterBalancerStablePoolParams`];
    /// this adapter registers it into [`PoolEntry::BalancerStable`].
    /// `invariant_version` overrides the Vault-specialization heuristic.
    /// Returns the `pool_id`.
    #[pyo3(signature = (address, vault, block=None, invariant_version=None))]
    fn build_balancer_stable_pool(
        &self,
        py: Python<'_>,
        address: &str,
        vault: &str,
        block: Option<u64>,
        invariant_version: Option<u8>,
    ) -> PyResult<u64> {
        use degenbot_bot::bot_core::pool_builder::builder;
        use degenbot_core::runtime::get_runtime;
        let addr = parse_address(address)?;
        let vault_addr = parse_address(vault)?;
        let io = self.bot.construction_io_arc().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "build_balancer_stable_pool: no ConstructionIo attached (requires an alloy provider)",
            )
        })?;
        let params = py
            .detach(|| {
                get_runtime().block_on(builder::build_balancer_stable(
                    vault_addr,
                    addr,
                    &io,
                    block,
                    invariant_version,
                ))
            })
            .map_err(map_builder_err)?;
        // Incident 2026-08-20 #2: never hold the GIL while parked on the
        // BotState write - the dispatch fan-out's per-candidate tasks hold
        // the read end across provider fetches, and a parked GIL-writer
        // freezes every GIL consumer (main asyncio, log drainer, gil-probe).
        // Evidence: /tmp/degenbot-gil-deadlock-2026-08-20 (26 readers, state 0x1b).
        Ok(self.with_state_mut(py, |s| s.register_balancer_stable_pool(&params)))
    }

    /// Build + register a V3 pool through the Rust `PoolBuilder` (T4 / 4GQWZ4
    /// delegation adapter) — the V3 twin of [`Self::build_v2_pool`]. The tick
    /// map is assembled DB-first (a `TickMapDb` hit → `Tracked`, feeding the
    /// IKGQ6F quarantine→verify lifecycle; `db=false` forces the Chain-arm
    /// Sparse path). `block` defaults to the current chain head.
    ///
    /// # Errors
    ///
    /// As [`Self::build_v2_pool`].
    #[pyo3(signature = (address, block=None, db=true, tick_data_fetcher=None))]
    fn build_v3_pool(
        &self,
        py: Python<'_>,
        address: &str,
        block: Option<u64>,
        db: bool,
        tick_data_fetcher: Option<Bound<'_, PyAny>>,
    ) -> PyResult<(u64, String, String, String, String)> {
        use degenbot_bot::bot_core::pool_builder::builder;
        use degenbot_core::runtime::get_runtime;
        let addr = parse_address(address)?;
        let io = self.bot.construction_io_arc().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "build_v3_pool: no ConstructionIo attached (requires an alloy provider)",
            )
        })?;
        let chain_id = self.bot.chain_id();
        let db_arc: Option<std::sync::Arc<degenbot_db::snapshot_db::SnapshotDb>> =
            if db { self.db_handle() } else { None };
        let db_ref: Option<&dyn degenbot_db::snapshot::TickMapDb> = db_arc
            .as_deref()
            .map(|d| d as &dyn degenbot_db::snapshot::TickMapDb);
        let mut params = py
            .detach(|| {
                get_runtime().block_on(builder::build_v3(chain_id, addr, db_ref, &io, block))
            })
            .map_err(map_builder_err)?;
        // ADR-005 sparse-map parity: `build_v3` registers a single tick word
        // (Sparse). The core builder leaves the backfill fetcher `None`; the
        // Python driver injects a `PyTickWordFetcher` wrapping the legacy
        // web3-sync `tick_data_fetcher` here (GIL held) so swap-time
        // boundary-detection can pull neighbouring words without re-entering
        // the asyncio runtime (a Rust `block_on` fetcher would deadlock).
        if let Some(fetcher) = tick_data_fetcher.filter(|f| !f.is_none()) {
            params.fetcher = Some(crate::bot::pool::make_tick_fetcher(fetcher.unbind()));
        }
        // TF7RZB-S1 (builder return surface): return the core-computed identity
        // alongside the pool_id. V3 has no per-pool `DexVariant`; the family is
        // resolved from the builder-verified `factory` via the Rust-owned
        // `resolve_dex_name` (kebab-case, e.g. "uniswap"), falling back to the
        // generic "uniswap-v3" when the factory is not a known deployment.
        let family = degenbot_uniswap::deployments::resolve_dex_name(chain_id, params.factory)
            .map_or_else(|| "uniswap-v3".to_string(), |d| d.as_str().to_string());
        let identity = (
            params.token0.to_checksum(None),
            params.token1.to_checksum(None),
            params.address.to_checksum(None),
            family,
        );
        // Incident 2026-08-20 #2: never hold the GIL while parked on the
        // BotState write - the dispatch fan-out's per-candidate tasks hold
        // the read end across provider fetches, and a parked GIL-writer
        // freezes every GIL consumer (main asyncio, log drainer, gil-probe).
        // Evidence: /tmp/degenbot-gil-deadlock-2026-08-20 (26 readers, state 0x1b).
        let pool_id = self
            .with_state_mut(py, |s| s.register_v3_pool(&params))
            .map_err(map_register_v3_err)?;
        Ok((pool_id, identity.0, identity.1, identity.2, identity.3))
    }

    /// Build a V4 pool via the Rust `PoolBuilder` (T4 / 4GQWZ4), registered
    /// into `BotState` via `register_v4_pool`.
    ///
    /// V4 identity is **caller-supplied** (mirrors `register_v4_pool`: the
    /// core never reads `getToken0`/`getHooks` on-chain). The builder fetches
    /// only the LIVE scalars (`getSlot0` + `getLiquidity` on `state_view`)
    /// and assembles/seeds a Sparse tick map via the Chain arm — reusing the
    /// StateView-backed choreography with no new ABI.
    ///
    /// The caller must register the V4 `state_view` mapping for
    /// `pool_manager` (done here, idempotent) before this returns, so the
    /// solver-state accuracy gate can verify V4 hops on-chain.
    ///
    /// # Errors
    ///
    /// As [`Self::register_v4_pool`] (builder RPC/decode errors map via
    /// [`map_builder_err`]; admission/filtering errors via
    /// [`map_register_v4_err`]).
    #[pyo3(signature = (chain_id, pool_manager, pool_id_hex, currency0=None, currency1=None, fee=None, tick_spacing=None, hook_address=None, state_view_address=None))]
    #[expect(clippy::too_many_arguments)]
    fn resolve_v4_identity(
        &self,
        py: Python<'_>,
        chain_id: u64,
        pool_manager: &str,
        pool_id_hex: &str,
        currency0: Option<&str>,
        currency1: Option<&str>,
        fee: Option<u32>,
        tick_spacing: Option<i32>,
        hook_address: Option<&str>,
        state_view_address: Option<&str>,
    ) -> PyResult<(String, String, u32, i32, u16, String)> {
        use degenbot_bot::bot_core::pool_builder::builder;
        use degenbot_core::runtime::get_runtime;
        let pm = parse_address(pool_manager).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "resolve_v4_identity: malformed pool_manager {pool_manager:?}: {e}"
            ))
        })?;
        let pool_id = hex_string_to_pool_id(pool_id_hex)?;
        let parse_opt = |s: Option<&str>, name: &str| -> PyResult<Option<Address>> {
            s.map(|s| {
                parse_address(s).map_err(|e| {
                    pyo3::exceptions::PyValueError::new_err(format!(
                        "resolve_v4_identity: malformed {name} {s:?}: {e}"
                    ))
                })
            })
            .transpose()
        };
        // TF7RZB-S3: the V4 identity (currency0/1, fee, tick_spacing, hook,
        // state_view) is resolved CORE-side — the DB two-step (manager → V4 row
        // → per-FK tokens) first, else these raw caller overrides. The override
        // fields are all optional; the resolver raises a typed `MissingIdentity`
        // (mapped to PyValueError) when neither source is complete.
        let overrides = builder::V4PoolBuildOverrides {
            currency0: parse_opt(currency0, "currency0")?,
            currency1: parse_opt(currency1, "currency1")?,
            fee,
            tick_spacing,
            hook_address: parse_opt(hook_address, "hook_address")?,
            state_view: parse_opt(state_view_address, "state_view_address")?,
        };
        let io = self.bot.construction_io_arc().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "resolve_v4_identity: no ConstructionIo attached (requires an alloy provider)",
            )
        })?;
        let id = py
            .detach(|| {
                get_runtime().block_on(builder::resolve_v4_identity(
                    chain_id, pm, pool_id, &overrides, &io,
                ))
            })
            .map_err(map_builder_err)?;
        Ok((
            id.currency0.to_checksum(None),
            id.currency1.to_checksum(None),
            id.fee,
            id.tick_spacing,
            id.hook_flags,
            id.state_view.to_checksum(None),
        ))
    }

    #[pyo3(signature = (pool_manager, pool_id_hex, currency0, currency1, fee, tick_spacing, hook_flags, state_view_address, block=None, db=true, tick_data_fetcher=None))]
    #[expect(clippy::too_many_arguments)]
    #[expect(clippy::type_complexity)]
    fn build_v4_pool(
        &self,
        py: Python<'_>,
        pool_manager: &str,
        pool_id_hex: &str,
        currency0: &str,
        currency1: &str,
        fee: u32,
        tick_spacing: i32,
        hook_flags: u16,
        state_view_address: &str,
        block: Option<u64>,
        db: bool,
        tick_data_fetcher: Option<Bound<'_, PyAny>>,
    ) -> PyResult<(
        u64,
        String,
        String,
        String,
        String,
        u32,
        i32,
        u16,
        String,
        u32,
        u32,
    )> {
        use degenbot_bot::bot_core::pool_builder::builder;
        use degenbot_core::runtime::get_runtime;
        let pm = parse_address(pool_manager).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "build_v4_pool: malformed pool_manager {pool_manager:?}: {e}"
            ))
        })?;
        let pool_id = hex_string_to_pool_id(pool_id_hex)?;
        let state_view = parse_address(state_view_address).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "build_v4_pool: malformed state_view_address {state_view_address:?}: {e}"
            ))
        })?;
        // Register the Rust-owned V4 StateView registry first (ADR-005 /
        // Option 2) — the solver-state gate reads it to verify V4 hops.
        {
            self.with_state_mut(py, |s| s.register_v4_state_view(pm, state_view));
        }
        let io = self.bot.construction_io_arc().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "build_v4_pool: no ConstructionIo attached (requires an alloy provider)",
            )
        })?;
        let db_arc: Option<std::sync::Arc<degenbot_db::snapshot_db::SnapshotDb>> =
            if db { self.db_handle() } else { None };
        let db_ref: Option<&dyn degenbot_db::snapshot::TickMapDb> = db_arc
            .as_deref()
            .map(|d| d as &dyn degenbot_db::snapshot::TickMapDb);
        let id = builder::V4PoolBuildIdentity {
            pool_manager: pm,
            state_view,
            pool_id,
            currency0: parse_address(currency0).map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "build_v4_pool: malformed currency0 {currency0:?}: {e}"
                ))
            })?,
            currency1: parse_address(currency1).map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "build_v4_pool: malformed currency1 {currency1:?}: {e}"
                ))
            })?,
            fee,
            tick_spacing,
            hook_flags,
        };
        // TF7RZB-S2 (builder return surface): capture the normalized identity
        // tuple before `id` is moved into `build_v4`.
        let identity_ret = (
            id.currency0.to_checksum(None),
            id.currency1.to_checksum(None),
            id.pool_manager.to_checksum(None),
            id.fee,
            id.tick_spacing,
            id.hook_flags,
            format!("0x{}", alloy::hex::encode(id.pool_id)),
        );
        let result = py
            .detach(|| get_runtime().block_on(builder::build_v4(id, db_ref, &io, block)))
            .map_err(map_builder_err)?;
        let mut params = result.params;
        // CDJEPJ-1: lp_fee + protocol_fee come from the SAME head-stamped slot0
        // read inside build_v4 (no second fetch_v4_slot0_liquidity each pool).
        let lp_fee = result.lp_fee;
        let protocol_fee = params.protocol_fee;
        // Sparse-map parity (V4 twin of `build_v3_pool`): `build_v4` leaves the
        // backfill fetcher `None`; inject the Python web3-sync fetcher here.
        if let Some(fetcher) = tick_data_fetcher.filter(|f| !f.is_none()) {
            params.fetcher = Some(crate::bot::pool::make_tick_fetcher(fetcher.unbind()));
        }
        let coverage = match params.coverage {
            degenbot_bot::bot_core::PoolTickCoverage::Tracked => "tracked",
            degenbot_bot::bot_core::PoolTickCoverage::Sparse => "sparse",
        };
        // Incident 2026-08-20 #2: never hold the GIL while parked on the
        // BotState write - the dispatch fan-out's per-candidate tasks hold
        // the read end across provider fetches, and a parked GIL-writer
        // freezes every GIL consumer (main asyncio, log drainer, gil-probe).
        // Evidence: /tmp/degenbot-gil-deadlock-2026-08-20 (26 readers, state 0x1b).
        let pool_id = self
            .with_state_mut(py, |s| s.register_v4_pool(&params))
            .map_err(map_register_v4_err)?;
        // Return `(pool_id, coverage, identity..., protocol_fee, lp_fee)` so the
        // Python driver can set the companion's `_sparse_liquidity_map` (from
        // coverage), the normalized identity, and the fee overrides
        // (protocol_fee/lp_fee from the builder's own slot0 read) in one return
        // surface (TF7RZB-S2 / CDJEPJ-1).
        Ok((
            pool_id,
            coverage.to_string(),
            identity_ret.0,
            identity_ret.1,
            identity_ret.2,
            identity_ret.3,
            identity_ret.4,
            identity_ret.5,
            identity_ret.6,
            protocol_fee,
            lp_fee,
        ))
    }

    /// Prototype test-only V2 registration that bypasses CREATE2 verification.
    /// Wire to a new method on Python so tests can build a synthetic V2 pool.
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn register_v2_pool_test_only(
        &self,
        address: &str,
        token0: &str,
        token1: &str,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        gamma_numer0: u64,
        fee_denom0: u64,
        gamma_numer1: u64,
        fee_denom1: u64,
        factory: &str,
        update_block: u64,
        variant: &str,
        stable_swap: bool,
        fee_denominator: Option<u64>,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let t0 = parse_address(token0)?;
        let t1 = parse_address(token1)?;
        let fac = parse_address(factory)?;
        let r0 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve0)?,
            "reserve0",
        )?;
        let r1 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve1)?,
            "reserve1",
        )?;
        let variant_enum = DexVariant::from_kebab(variant).ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err(format!("unknown variant: {variant}"))
        })?;
        // T1-scan-exempt: test-only seam (pure-Rust test callers, no GIL held).
        self.bot
            .state_arc()
            .write()
            .register_v2_pool(&RegisterV2PoolParams {
                address: addr,
                token0: t0,
                token1: t1,
                reserve0: r0,
                reserve1: r1,
                fee_token0: (gamma_numer0, fee_denom0),
                fee_token1: (gamma_numer1, fee_denom1),
                factory: fac,
                deployer: fac,
                init_hash: alloy::primitives::B256::default(),
                update_block,
                variant: variant_enum,
                stable_swap,
                fee_denominator,
            })
            .map_err(map_register_v2_err)
    }

    /// Update a V2 pool's reserves from a Sync event.
    #[pyo3(signature = (address, reserve0, reserve1, block_number))]
    fn update_v2_pool(
        &self,
        py: Python<'_>,
        address: &str,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        block_number: u64,
    ) -> PyResult<()> {
        let addr = parse_address(address)?;
        let r0 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve0)?,
            "reserve0",
        )?;
        let r1 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve1)?,
            "reserve1",
        )?;

        self.with_state_mut(py, |s| s.update_v2_pool(addr, r0, r1, block_number));
        Ok(())
    }

    /// Calculate the output token amount for a given input amount.
    ///
    /// Raises `ValueError` when the swap math overflows a `uint256` intermediate
    /// (mirrors on-chain `getAmountOut` `SafeMath` revert — cdbc03bb). A V3/V4
    /// sparse-map miss is still mapped to 0; callers needing the miss surfaced
    /// use `calculate_tokens_out_with_fetch` on the pool handle.
    #[pyo3(signature = (pool_id, zero_for_one, amount_in))]
    fn calculate_tokens_out(
        &self,
        py: Python<'_>,
        pool_id: u64,
        zero_for_one: bool,
        amount_in: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_in)?;
        // GIL hygiene: read guard acquired inside py.detach (inversion class).
        let result = self.with_state(py, |s| {
            s.calculate_tokens_out_miss_aware(pool_id, zero_for_one, amount)
        });
        // cdbc03bb: surface `NotComputable` (uint256 overflow = on-chain revert)
        // as a Python `ValueError` so the companion can translate it to a
        // domain `LiquidityPoolError`; keep `MissingTickWord` mapped to 0 so
        // callers that haven't opted into the fetch-retry path keep the legacy
        // no-raise-on-miss contract.
        let out = match result {
            Ok(v) => v,
            Err(SimulateSwapError::MissingTickWord(_)) => alloy::primitives::U256::ZERO,
            Err(SimulateSwapError::NotComputable) => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "Pool swap math overflowed uint256 intermediate (on-chain getAmountOut SafeMath revert)",
                ));
            }
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &out)?;
        Ok(bound.unbind())
    }

    /// Calculate the raw stableswap `get_dy` for a Curve pool (Rust-owned;
    /// task `45QBUG`, epic `TV72EG`). Backs the companion `CurveStableswapPool
    /// .get_dy` so no Python provider / cache / calculator is on the path.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_curve_pool`.
    ///     `i`/`j`: Coin indices.
    ///     `dx`: Input amount (Python int).
    ///     `block_number`: Block to resolve against.
    ///     `override_balances`: Optional `list[int]` to use as the balance
    ///         source instead of the pool's current balances.
    ///
    /// Returns:
    ///     The raw `dy` output amount as a Python int.
    #[expect(clippy::too_many_arguments)]
    #[pyo3(signature = (pool_id, i, j, dx, block_number, override_balances=None))]
    fn curve_get_dy(
        &self,
        py: Python<'_>,
        pool_id: u64,
        i: usize,
        j: usize,
        dx: &Bound<'_, PyAny>,
        block_number: u64,
        override_balances: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::conversion::alloy::extract_python_u256(dx)?;
        let overrides = match override_balances {
            Some(list) => {
                let cast = list.cast::<PyList>().map_err(|_| {
                    pyo3::exceptions::PyTypeError::new_err("override_balances must be a list[int]")
                })?;
                Some(extract_u256_list(cast)?)
            }
            None => None,
        };
        // GIL hygiene: read guard acquired inside py.detach (inversion class).
        let result = self.with_state(py, |s| {
            s.curve_get_dy(pool_id, i, j, amount, block_number, overrides.as_deref())
        });
        let out = match result {
            Ok(v) => v,
            Err(e) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!("{e:?}")));
            }
        };
        let bound = crate::conversion::alloy::u256_to_py(py, &out)?;
        Ok(bound.unbind())
    }

    /// Calculate the required input token amount for a given output amount.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_v2_pool`.
    ///     `zero_for_one`: True for token0→token1, False for token1→token0.
    ///     `amount_out`: Desired output token amount (Python int).
    ///
    /// Returns:
    ///     The required input token amount as a Python int.
    #[pyo3(signature = (pool_id, zero_for_one, amount_out))]
    fn calculate_tokens_in(
        &self,
        py: Python<'_>,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
        // GIL hygiene: read guard acquired inside py.detach (inversion class).
        let result = self.with_state(py, |s| s.calculate_tokens_in(pool_id, zero_for_one, amount));
        let bound = crate::conversion::alloy::u256_to_py(py, &result)?;
        Ok(bound.unbind())
    }

    /// Number of registered pools.
    fn pool_count(&self, py: Python<'_>) -> usize {
        // GIL hygiene: read guard acquired inside py.detach (inversion class).
        self.with_state(py, degenbot_bot::bot_core::BotState::pool_count)
    }

    /// The chain this `PyBot` orchestrates (ADR-006 D4). Wired from the
    /// single-chain Python `Bot` facade's ``config.default_chain_id``; `0` on
    /// the bare-test-fixture path (see `PyBot::new`).
    #[getter]
    fn chain_id(&self) -> u64 {
        self.bot.chain_id()
    }

    /// Drive a raw WS log through the event bus (ADR-006 D4).
    ///
    /// This is the Python-facing mirror of the `BlockPump`'s per-log call to
    /// `Bot::dispatch_log`: decode via the registered `LogDecoder`s, apply the
    /// decoded event to the shared `BotState` under a write guard, release it,
    /// then notify every attached `PoolStateSubscriber` (the engine adapter) so
    /// the affected `pool_id` is dirtied for the next `solve_all_paths`.
    ///
    /// Reconstructs an `alloy::rpc::types::Log` from the WS-log shape Python
    /// passes — `(address, topics, data, block_number)` — so an offline test
    /// can drive the full pump→dispatch→notify→solve loop without a live node.
    /// No-op if no decoder recognizes the log or the pool isn't registered.
    ///
    /// Args:
    ///     `address`: Emitter contract address (hex string, `0x` optional).
    ///     `topics`: Log topics (hex strings, `0x` optional). `topics[0]` is
    ///         the event signature hash the decoders match against.
    ///     `data`: ABI-encoded log data (hex string, `0x` optional).
    ///     `block_number`: Block number the synthetic log belongs to.
    #[pyo3(signature = (address, topics, data, block_number=0))]
    fn dispatch_log(
        &self,
        address: &str,
        topics: Vec<String>,
        data: &str,
        block_number: u64,
    ) -> PyResult<()> {
        let log = build_rpc_log(address, topics, data, block_number)?;
        self.bot.dispatch_log(&log);
        Ok(())
    }

    /// Get a thin `PyLiquidityPool` handle for the given pool ID.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_v2_pool`.
    ///
    /// Returns:
    ///     A `PyLiquidityPool` handle, or `None` if the pool ID is not registered.
    fn get_pool(&self, py: Python<'_>, pool_id: u64) -> Option<PyLiquidityPool> {
        // GIL hygiene: read guard acquired inside py.detach (inversion class).
        if self.with_state(py, |s| s.has_pool(pool_id)) {
            Some(PyLiquidityPool::new(self.bot.state_arc(), pool_id))
        } else {
            None
        }
    }

    /// Prototype structural pool handle (V2 slice).
    fn py_pool(&self, py: Python<'_>, pool_id: u64) -> Option<crate::bot::pool::PyPool> {
        // GIL hygiene: read guard acquired inside py.detach (inversion class).
        if self.with_state(py, |s| s.has_pool(pool_id)) {
            Some(crate::bot::pool::PyPool::new(
                self.bot.state_arc(),
                pool_id,
                self.bot.chain_id(),
            ))
        } else {
            None
        }
    }

    /// Unregister a V2/V3 pool by its contract address.
    ///
    /// ADR-007 U3. Drops the `PoolEntry`, its reorg journal, its index entry,
    /// and any buffered V3 liquidity events for the address so a re-register
    /// does not replay stale Mint/Burn onto the fresh pool. `next_pool_id` is
    /// not reused — removed ids are retired to prevent stale `PyLiquidityPool`
    /// handles aliasing to a different pool on recreate.
    ///
    /// V2/V3 path only — `PyBot` exposes `register_v2/v3_pool` (no V4;
    /// V4 registration lives on `ArbitrageEngine`, and its symmetric
    /// unregister belongs there too — see ADR-007 Consequences).
    ///
    /// Returns `True` if a pool was found and removed; `False` if the address
    /// was never registered (silent no-op, mirroring Python `PoolRegistry.remove`).
    /// Register stays refusal-on-panic for duplicates (ADR-007 U2) — the
    /// asymmetry reflects the asymmetry in the operations' invariants.
    ///
    /// Args:
    ///     `address`: The V2/V3 pool contract address (checksum or 0x-hex).
    ///     `pool_id`: Reserved — must be `None` on `PyBot`. (V4 tuple-key
    ///         unregister is engine-side; kept in the signature for parity
    ///         with `BotState::unregister_pool`.)
    ///
    /// Raises:
    ///     `ValueError`: If `address` cannot be parsed.
    #[pyo3(signature = (address, pool_id=None))]
    #[expect(clippy::needless_pass_by_value)] // PyO3 binding idiom for optional bytes
    fn unregister_pool(
        &self,
        py: Python<'_>,
        address: &str,
        pool_id: Option<Vec<u8>>,
    ) -> PyResult<bool> {
        let addr = parse_address(address)?;
        // V4 on PyBot is intentionally not exposed: registration for V4 lives
        // on ArbitrageEngine (bot::engine), so the symmetric V4 unregister
        // belongs there too (ADR-007 Consequences / Deferred). A `Some` here
        // would be a caller bug — surface it rather than silently no-op.
        if pool_id.is_some() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "Bot.unregister_pool does not handle V4 (pool_id set); \
                 V4 unregister is engine-side — use the engine’s unregister path.",
            ));
        }
        // Incident 2026-08-20 (GIL/BotState inversion): never hold the GIL
        // while parked on the BotState write - the dispatch fan-out reader
        // can hold the read end across provider RPCs for seconds, and a
        // parked GIL-writer freezes the main asyncio loop (the observed
        // 'GIL deadlock').
        Ok(self.with_state_mut(py, |s| s.unregister_pool(addr, None)))
    }

    /// Assemble a V3 pool's tick map from the stored DB snapshot (`Store → Db`
    /// precedence — epic UHPXSD / Decision 6 (B)). Returns
    /// `(tick_data, coverage)` on a hit, `None` on a miss (caller runs Branch 3
    /// sparse RPC and registers inline). Raises `RuntimeError` on a Db read
    /// failure — Decision 8 (A): loud error over silent degrade.
    ///
    /// `tick_data` has the same `{tick: (liquidity_gross, liquidity_net, block)}`
    /// shape as `register_v3_pool`'s `tick_data` arg, so the builder can pass
    /// the returned dict straight back into `register_v3_pool`. Coverage is
    /// `"tracked"` on a hit (Store or Db).
    ///
    /// **Two-phase lock protocol**: `BotState`'s read guard is held only for
    /// the `SnapshotStore::take` (brief `Mutex` lock); the Db read inside the
    /// core helper runs with NO `BotState` lock held (the live pump's
    /// `state.write()` is not blocked). The GIL is released across the Db read.
    ///
    /// Args:
    ///   address: the V3 pool's contract address (checksummed or lowercase hex).
    ///   tick: the pool's current tick (drives the Chain arm's word computation).
    ///   `tick_spacing`: the pool's tick spacing (drives active-tick computation).
    ///   block: the snapshot block for the Chain arm's RPC reads.
    ///   io: optional `PyBotIo` whose native alloy provider backs the Chain arm.
    ///     `None` (or a non-alloy provider) → no Chain arm (Store + Db only).
    #[pyo3(signature = (address, tick=0, tick_spacing=0, block=0, io=None))]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "PyO3 passes the pyclass Bound by value; borrow is internal"
    )]
    fn assemble_v3_tick_map<'py>(
        &self,
        py: Python<'py>,
        address: &str,
        tick: i64,
        tick_spacing: i64,
        block: u64,
        io: Option<pyo3::Bound<'_, crate::bot::py_bot_io::PyBotIo>>,
    ) -> PyResult<Option<(Bound<'py, PyDict>, String)>> {
        let addr = parse_address(address)?;
        let tick_i32 = i32::try_from(tick)
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("tick out of int24 range"))?;
        let spacing_i32 = i32::try_from(tick_spacing)
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("tick_spacing out of range"))?;
        let db = self.db_handle();
        // NOD4PS: extract the native alloy provider from `io` + construct an
        // `AlloyTickBootstrapRpc` (Option B — pure-Rust choreography, no GIL
        // re-entry per RPC). `None` when `io=None` or the provider isn't
        // alloy-backed → Chain arm short-circuits (current behavior).
        let chain: Option<std::sync::Arc<dyn degenbot_pools::tick_fetch::TickBootstrapRpc>> =
            io.as_ref().and_then(|io_bound| {
                io_bound
                    .try_borrow()
                    .ok()
                    .and_then(|borrowed| crate::bot::pool::make_tick_bootstrap_rpc(&borrowed))
            });
        let result = py.detach(|| {
            degenbot_bot::bot_core::tick_assembly::assemble_v3_tick_map(
                db.as_deref()
                    .map(|d| d as &dyn degenbot_db::snapshot::TickMapDb),
                addr,
                tick_i32,
                spacing_i32,
                block,
                chain.as_deref(),
            )
        });
        let Some((ticks, coverage)) = result.map_err(|e| crate::db::assembly_err_to_py(&e))? else {
            return Ok(None);
        };
        let dict = build_tick_rows_py(py, &ticks)?;
        let cov_str = match coverage {
            degenbot_bot::bot_core::PoolTickCoverage::Tracked => "tracked",
            degenbot_bot::bot_core::PoolTickCoverage::Sparse => "sparse",
        };
        Ok(Some((dict, cov_str.to_string())))
    }

    /// Assemble a V4 pool's tick map from the stored DB snapshot — V4 twin of
    /// [`Self::assemble_v3_tick_map`]. Same precedence, miss, and error
    /// semantics; returns `(tick_data, coverage)` on a hit, `None` on miss,
    /// `RuntimeError` on Db error.
    ///
    /// Args:
    ///   `pool_manager`: the V4 `PoolManager` contract address (Db key).
    ///   `pool_id`: the V4 pool id as a 32-byte hex `str` (`0x...`) or `bytes`.
    ///   `state_view`: the V4 `StateView` contract address (Chain-arm RPC target).
    ///   `tick`/`tick_spacing`/`block`/`io`: see [`Self::assemble_v3_tick_map`].
    #[pyo3(signature = (pool_manager, pool_id, state_view, tick=0, tick_spacing=0, block=0, io=None))]
    #[expect(
        clippy::too_many_arguments,
        clippy::needless_pass_by_value,
        reason = "PyO3 wrapper over the precedence helper; signature mirrors Python surface"
    )]
    fn assemble_v4_tick_map<'py>(
        &self,
        py: Python<'py>,
        pool_manager: &str,
        pool_id: &Bound<'_, PyAny>,
        state_view: &str,
        tick: i64,
        tick_spacing: i64,
        block: u64,
        io: Option<pyo3::Bound<'_, crate::bot::py_bot_io::PyBotIo>>,
    ) -> PyResult<Option<(Bound<'py, PyDict>, String)>> {
        let mgr = parse_address(pool_manager)?;
        let state_view_addr = parse_address(state_view)?;
        let tick_i32 = i32::try_from(tick)
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("tick out of int24 range"))?;
        let spacing_i32 = i32::try_from(tick_spacing)
            .map_err(|_| pyo3::exceptions::PyValueError::new_err("tick_spacing out of range"))?;
        // V4 pool id is `B256` ([u8; 32]); accept either a 0x-hex str or 32
        // bytes (mirror `db/snapshot.rs::get_liquidity_map_v4`'s arg accept).
        let pool_id_hash = if let Ok(s) = pool_id.extract::<String>() {
            <alloy::primitives::B256 as std::str::FromStr>::from_str(&s)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("bad pool_id: {e}")))?
        } else if let Ok(bytes) = pool_id.extract::<&[u8]>() {
            if bytes.len() != 32 {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "V4 pool_id bytes must be exactly 32 long, got {}",
                    bytes.len()
                )));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(bytes);
            alloy::primitives::B256::from(arr)
        } else {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "pool_id must be a 0x-hex str or 32-byte bytes",
            ));
        };
        let db = self.db_handle();
        let pool_id_inner: degenbot_decoders::v4_swap_decoder::V4PoolId = pool_id_hash.0; // B256 = FixedBytes<32>; .0 is [u8; 32]
        let chain: Option<std::sync::Arc<dyn degenbot_pools::tick_fetch::TickBootstrapRpc>> =
            io.as_ref().and_then(|io_bound| {
                io_bound
                    .try_borrow()
                    .ok()
                    .and_then(|borrowed| crate::bot::pool::make_tick_bootstrap_rpc(&borrowed))
            });
        let result = py.detach(|| {
            degenbot_bot::bot_core::tick_assembly::assemble_v4_tick_map(
                db.as_deref()
                    .map(|d| d as &dyn degenbot_db::snapshot::TickMapDb),
                mgr,
                state_view_addr,
                pool_id_inner,
                tick_i32,
                spacing_i32,
                block,
                chain.as_deref(),
            )
        });
        let Some((ticks, coverage)) = result.map_err(|e| crate::db::assembly_err_to_py(&e))? else {
            return Ok(None);
        };
        let dict = build_tick_rows_py(py, &ticks)?;
        let cov_str = match coverage {
            degenbot_bot::bot_core::PoolTickCoverage::Tracked => "tracked",
            degenbot_bot::bot_core::PoolTickCoverage::Sparse => "sparse",
        };
        Ok(Some((dict, cov_str.to_string())))
    }

    /// Register a V3 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    #[expect(clippy::too_many_arguments)]
    /// Register a V3 pool, optionally seeding `tick_data` inline.
    ///
    /// ADR-006 rolling-start race closure: the builder previously called
    /// ``register_v3_pool`` (empty `tick_data`) THEN ``update_tick_data`` to seed
    /// the snapshot. Because ``resume()`` runs before ``build_paths``, a pump
    /// Mint/Burn landing between register and seed was applied to the empty
    /// `tick_data` then CLOBBERED by the seed overwrite — a lost update the
    /// on-chain ``verify_liquidity_maps`` reproduces. Folding the seed into
    /// ``register_v3_pool`` (one ``BotState`` write lock) closes the window:
    /// the pool is never visible to the pump in an unseeded state, so pump
    /// events always land on top of the seed and are never overwritten.
    ///
    /// `tick_data` is ``{tick: (liquidity_gross, liquidity_net, block)}``
    /// (symmetric with ``PyLiquidityPool.tick_data_snapshot``).
    /// `coverage` is ``"tracked"`` (complete DB snapshot) or ``"sparse"``
    /// (RPC-fetched active word only / no snapshot).
    #[pyo3(signature = (address, token0, token1, fee, tick_spacing, factory, sqrt_price_x96, liquidity, tick, tick_data=None, update_block=0, coverage="sparse", tick_data_fetcher=None, tick_data_block=None))]
    fn register_v3_pool(
        &self,
        py: Python<'_>,
        address: &str,
        token0: &str,
        token1: &str,
        fee: u32,
        tick_spacing: i32,
        factory: &str,
        sqrt_price_x96: &Bound<'_, PyAny>,
        liquidity: &Bound<'_, PyAny>,
        tick: i32,
        tick_data: Option<Bound<'_, PyDict>>,
        update_block: u64,
        coverage: &str,
        tick_data_fetcher: Option<Bound<'_, PyAny>>,
        tick_data_block: Option<u64>,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let t0 = parse_address(token0)?;
        let t1 = parse_address(token1)?;
        let fac = parse_address(factory)?;
        let spx = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;
        // liquidity is uint128 — extracted as U256 then narrowed.
        let liq = crate::conversion::alloy::extract_python_u256(liquidity)?.to::<u128>();

        // Convert `{tick: (gross, net, block)}` → `HashMap<i32, TickInfo>`
        // (mirrors `PyLiquidityPool.update_tick_data`'s boundary conversion).
        let rust_tick_data: std::collections::HashMap<i32, degenbot_bot::bot_core::TickInfo> =
            match tick_data {
                Some(dict) => {
                    let parsed: std::collections::HashMap<i32, (u128, i128, u64)> =
                        dict.extract().map_err(|_| {
                            pyo3::exceptions::PyTypeError::new_err(
                                "register_v3_pool: tick_data must be {tick: (gross, net, block)}",
                            )
                        })?;
                    parsed
                        .into_iter()
                        .map(|(tick, (gross, net, blk))| {
                            (
                                tick,
                                degenbot_bot::bot_core::TickInfo {
                                    liquidity_gross: alloy::primitives::U128::from(gross),
                                    liquidity_net: alloy::primitives::I256::try_from(net)
                                        .unwrap_or(alloy::primitives::I256::ZERO),
                                    block: blk,
                                },
                            )
                        })
                        .collect()
                }
                None => std::collections::HashMap::new(),
            };
        let cov = match coverage {
            "tracked" => PoolTickCoverage::Tracked,
            "sparse" => PoolTickCoverage::Sparse,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "register_v3_pool: coverage must be 'tracked' or 'sparse', got {other:?}",
                )));
            }
        };

        // Verify the pool address against the JSON-sourced CREATE2 deployer +
        // init hash (Fork A, JC6OFG). Skipped if (chain, factory) is not in the
        // shipped JSON — preserves the manual/ad-hoc registration path.
        crate::bot::deployments::verify_v3(self.bot.chain_id(), fac, addr, t0, t1, fee)?;

        // Resolve the JSON-sourced CREATE2 deployer + init hash for this
        // (chain, factory) (Fork A, P62DKO). Stored on the pool identity so the
        // Python companion reads it off the handle. Non-JSON pools default to
        // factory-as-deployer + the Uniswap V3 mainnet fallback init hash.
        let chain_id = self.bot.chain_id();
        let deployer = degenbot_uniswap::deployments::resolve_deployer(chain_id, fac);
        let init_hash_b256 = degenbot_uniswap::deployments::resolve_v3_init_hash(chain_id, fac);

        let params = RegisterV3PoolParams {
            address: addr,
            token0: t0,
            token1: t1,
            fee,
            tick_spacing,
            factory: fac,
            deployer,
            init_hash: init_hash_b256,
            sqrt_price_x96: spx,
            liquidity: liq,
            tick,
            tick_data: rust_tick_data,
            update_block,
            tick_data_block,
            coverage: cov,
            fetcher: tick_data_fetcher
                .filter(|f| !f.is_none())
                .map(|f| crate::bot::pool::make_tick_fetcher(f.clone().unbind())),
        };
        // YLYJM2: the write-lock acquisition + `register_v3_pool` run inside
        // the accessor's py.detach so the live pump + asyncio loop keep making GIL
        // progress while the main thread awaits `core.write()` (the startup-
        // stall enabler — see `register_token`). `RegisterV3PoolParams` is
        // `Send` (all-`Send` fields; `fetcher` is `Option<Arc<dyn
        // TickWordFetcher>>`, `TickWordFetcher: Send + Sync`); the error is
        // mapped to a `PyErr` OUTSIDE the closure (GIL-held) since `PyErr`
        // construction needs the interpreter.
        let result = self.with_state_mut(py, |s| s.register_v3_pool(&params));
        result.map_err(map_register_v3_err)
    }

    /// Register a V4 pool by `(pool_manager, pool_id)`.
    ///
    /// Returns the auto-assigned pool ID. The hook + dynamic-fee admission
    /// floor lives in `BotState::register_v4_pool` (ADR-005 slice 9a): pools
    /// with amount-modifying hooks (`hook_flags & 0xCC != 0`), dynamic fees
    /// (`fee == 0x100000`), or static fees exceeding the executor's 2-byte
    /// encoding limit (`fee > 65535`) are rejected here, surfacing as typed
    /// Python exceptions (`HookedPoolRejectedError` /
    /// `DynamicFeePoolRejectedError` / `HighFeePoolRejectedError`) so Python
    /// classifies by type, not string matching.
    ///
    /// ADR-006 rolling-start race closure: the snapshot `tick_data` is seeded
    /// INLINE in `register_v4_pool` (one `BotState` write lock) so the pool is
    /// never visible to the live pump (resumed before `build_paths`) in an
    /// unseeded state. Previously the builder registered with empty `tick_data`
    /// then called `update_tick_data` to seed — a pump `ModifyLiquidity` landing
    /// between register and seed was applied then CLOBBERED by the seed
    /// overwrite (lost update → V4 tick-map desync → `verify_liquidity_maps`
    /// mismatch). Mirrors the V3 closure.
    ///
    /// Raises:
    ///     `HookedPoolRejectedError`: If `hook_flags & 0xCC != 0`.
    ///     `DynamicFeePoolRejectedError`: If `fee == 0x100000`.
    ///     `HighFeePoolRejectedError`: If `fee > 65535` (executor can't encode).
    ///     `ValueError`: If `addresses/pool_id` are malformed or already
    ///         registered.
    #[expect(clippy::too_many_arguments)]
    #[pyo3(signature = (pool_manager, pool_id_hex, currency0, currency1, fee, tick_spacing, hook_flags, sqrt_price_x96, liquidity, tick, block, tick_data=None, coverage="sparse", tick_data_fetcher=None, protocol_fee=0, tick_data_block=None))]
    fn register_v4_pool(
        &self,
        py: Python<'_>,
        pool_manager: &str,
        pool_id_hex: &str,
        currency0: &str,
        currency1: &str,
        fee: u32,
        tick_spacing: i32,
        hook_flags: u16,
        sqrt_price_x96: &Bound<'_, PyAny>,
        liquidity: u128,
        tick: i32,
        block: u64,
        tick_data: Option<Bound<'_, pyo3::types::PyDict>>,
        coverage: &str,
        tick_data_fetcher: Option<Bound<'_, PyAny>>,
        protocol_fee: u32,
        tick_data_block: Option<u64>,
    ) -> PyResult<u64> {
        let pm = pool_manager.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager address: {e}"))
        })?;
        let pool_id = hex_string_to_pool_id(pool_id_hex)?;
        let c0 = currency0.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid currency0 address: {e}"))
        })?;
        let c1 = currency1.parse::<Address>().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid currency1 address: {e}"))
        })?;
        let sp = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;
        // Convert `{tick: (gross, net, block)}` → `HashMap<i32, TickInfo>` —
        // mirrors `register_v3_pool` (ADR-006 rolling-start race closure for
        // V4: seed inline so the pool is never visible to the live pump
        // unseeded).
        let rust_tick_data: std::collections::HashMap<i32, degenbot_bot::bot_core::TickInfo> =
            match tick_data {
                Some(dict) => {
                    let parsed: std::collections::HashMap<i32, (u128, i128, u64)> =
                        dict.extract().map_err(|_| {
                            pyo3::exceptions::PyTypeError::new_err(
                                "register_v4_pool: tick_data must be {tick: (gross, net, block)}",
                            )
                        })?;
                    parsed
                        .into_iter()
                        .map(|(tick, (gross, net, blk))| {
                            (
                                tick,
                                degenbot_bot::bot_core::TickInfo {
                                    liquidity_gross: alloy::primitives::U128::from(gross),
                                    liquidity_net: alloy::primitives::I256::try_from(net)
                                        .unwrap_or(alloy::primitives::I256::ZERO),
                                    block: blk,
                                },
                            )
                        })
                        .collect()
                }
                None => std::collections::HashMap::new(),
            };
        let cov = match coverage {
            "tracked" => PoolTickCoverage::Tracked,
            "sparse" => PoolTickCoverage::Sparse,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "register_v4_pool: coverage must be 'tracked' or 'sparse', got {other:?}",
                )));
            }
        };
        let params = RegisterV4PoolParams {
            pool_manager: pm,
            pool_id,
            pool_key: V4PoolKey {
                currency0: c0,
                currency1: c1,
                fee,
                tick_spacing,
                hooks: Address::ZERO, // Hook filtering already done via hook_flags.
            },
            hook_flags,
            protocol_fee,
            sqrt_price_x96: sp,
            liquidity,
            tick,
            tick_data: rust_tick_data,
            update_block: block,
            tick_data_block,
            coverage: cov,
            fetcher: tick_data_fetcher
                .filter(|f| !f.is_none())
                .map(|f| crate::bot::pool::make_tick_fetcher(f.clone().unbind())),
        };
        // YLYJM2: the write-lock acquisition + `register_v4_pool` (see
        // `register_token`) run inside the accessor's py.detach. `RegisterV4PoolParams` is
        // `Send`; the error is mapped to a `PyErr` OUTSIDE the closure.
        let result = self.with_state_mut(py, |s| s.register_v4_pool(&params));
        result.map_err(map_register_v4_err)
    }

    /// Seed the Rust-owned V4 `StateView` registry (ADR-005 / Option 2):
    /// record the canonical `StateView` contract whose `getSlot0(bytes32)` /
    /// `getLiquidity(bytes32)` read a `pool_manager`'s pool state on-chain.
    /// Called once per `pool_manager` (the driver reads `state_view` from the
    /// `pool_managers` DB row) before V4 pools solve, so the solver-state
    /// accuracy gate can verify each V4 hop's state against the chain.
    ///
    /// Raises:
    ///     `ValueError`: If either address is malformed.
    fn register_v4_state_view(
        &self,
        py: Python<'_>,
        pool_manager: &str,
        state_view: &str,
    ) -> PyResult<()> {
        let pm = parse_address(pool_manager).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "register_v4_state_view: malformed pool_manager {pool_manager:?}: {e}"
            ))
        })?;
        let sv = parse_address(state_view).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "register_v4_state_view: malformed state_view {state_view:?}: {e}"
            ))
        })?;
        self.with_state_mut(py, |s| s.register_v4_state_view(pm, sv));
        Ok(())
    }

    /// Register a Curve `StableSwap` pool by contract address.
    ///
    /// Returns the auto-assigned pool ID. The `tokens` / `rate_multipliers` /
    /// `balances` lists must all have the same length `N` (the coin count).
    ///
    /// Raises:
    ///     `ValueError`: If the pool address is malformed or already
    ///         registered, or the list lengths mismatch.
    #[expect(clippy::too_many_arguments)]
    // `y_variant` / `yd_variant` mirror the public Python kwargs and the
    // `resolve_y_variant` / `resolve_yd_variant` vocabulary in
    // `degenbot.curve._variant_groups`; they are intentional domain terms,
    // not a naming slip, so the `similar_names` nudge does not apply here.
    #[expect(clippy::similar_names)]
    #[pyo3(signature = (
        address,
        tokens,
        a_coefficient,
        a_precision,
        fee,
        admin_fee,
        rate_multipliers,
        balances,
        update_block,
        swap_style=0,
        lending_rate_style=0,
        d_variant=0,
        y_variant=0,
        yd_variant=0,
        base_pool=None,
        initial_a_coefficient=None,
        future_a_coefficient=None,
        initial_a_coefficient_time=None,
        future_a_coefficient_time=None,
        create_timestamp=None,
        fee_gamma=None,
        mid_fee=None,
        offpeg_fee_multiplier=None,
        out_fee=None,
        gamma=None,
        lp_token=None,
        use_lending=None,
        precision_multipliers=None,
        tokens_underlying=None,
        metapool_rate_style=1,
        metapool_underlying_style=1,
        data_provider=None
    ))]
    fn register_curve_pool(
        &self,
        py: Python<'_>,
        address: &str,
        tokens: &Bound<'_, PyList>,
        a_coefficient: u128,
        a_precision: u128,
        fee: u64,
        admin_fee: u64,
        rate_multipliers: &Bound<'_, PyList>,
        balances: &Bound<'_, PyList>,
        update_block: u64,
        swap_style: u8,
        lending_rate_style: u8,
        d_variant: u8,
        y_variant: u8,
        yd_variant: u8,
        base_pool: Option<&str>,
        initial_a_coefficient: Option<u128>,
        future_a_coefficient: Option<u128>,
        initial_a_coefficient_time: Option<u64>,
        future_a_coefficient_time: Option<u64>,
        create_timestamp: Option<u64>,
        fee_gamma: Option<u64>,
        mid_fee: Option<u64>,
        offpeg_fee_multiplier: Option<u64>,
        out_fee: Option<u64>,
        gamma: Option<u64>,
        lp_token: Option<&str>,
        use_lending: Option<&Bound<'_, PyList>>,
        precision_multipliers: Option<&Bound<'_, PyList>>,
        tokens_underlying: Option<&Bound<'_, PyList>>,
        metapool_rate_style: u8,
        metapool_underlying_style: u8,
        data_provider: Option<Bound<'_, PyAny>>,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let token_addrs = parse_address_list(tokens)?;
        let rate_mults = extract_u256_list(rate_multipliers)?;
        let bal_vals = extract_u256_list(balances)?;
        let base = match base_pool {
            Some(s) => Some(parse_address(s)?),
            None => None,
        };
        let lp = match lp_token {
            Some(s) => Some(parse_address(s)?),
            None => None,
        };
        let use_lend: Vec<bool> = match use_lending {
            Some(l) => l.extract()?,
            None => Vec::new(),
        };
        let prec_mults: Vec<alloy::primitives::U256> = match precision_multipliers {
            Some(l) => extract_u256_list(l)?,
            None => Vec::new(),
        };
        let tokens_under = match tokens_underlying {
            Some(l) => Some(parse_address_list(l)?),
            None => None,
        };
        let p = RegisterCurvePoolParams {
            address: addr,
            tokens: token_addrs,
            a_coefficient,
            a_precision,
            fee,
            admin_fee,
            rate_multipliers: rate_mults,
            balances: bal_vals,
            update_block,
            swap_style,
            lending_rate_style,
            d_variant,
            y_variant,
            yd_variant,
            base_pool: base,
            initial_a_coefficient,
            future_a_coefficient,
            initial_a_coefficient_time,
            future_a_coefficient_time,
            create_timestamp,
            fee_gamma,
            mid_fee,
            offpeg_fee_multiplier,
            out_fee,
            gamma,
            lp_token: lp,
            use_lending: use_lend,
            precision_multipliers: prec_mults,
            tokens_underlying: tokens_under,
            metapool_rate_style,
            metapool_underlying_style,
            data_provider: data_provider
                .map(|b| crate::bot::pool::make_curve_data_provider(b.unbind())),
        };
        // Incident 2026-08-20 #2: never hold the GIL while parked on the
        // BotState write - the dispatch fan-out's per-candidate tasks hold
        // the read end across provider fetches, and a parked GIL-writer
        // freezes every GIL consumer (main asyncio, log drainer, gil-probe).
        // Evidence: /tmp/degenbot-gil-deadlock-2026-08-20 (26 readers, state 0x1b).
        Ok(self.with_state_mut(py, |s| s.register_curve_pool(&p)))
    }

    /// Build + register a Curve `StableSwap` pool through the Rust `PoolBuilder`
    /// (WKKMJM delegation adapter) — the Curve twin of [`Self::build_v2_pool`].
    /// The core `builder::build_curve_pool` runs the full detection
    /// choreography (coins + balances, `A`/`fee`/`admin_fee`, A-ramping,
    /// lending, crypto params, `lp_token`, metapool base + underlying coins,
    /// ERC20 decimals) and constructs a Rust `RpcCurveDataProvider` (V5F3DZ) —
    /// so the pool is registered with a **Rust-native** on-chain data
    /// provider, not a Python `CurveDataProviderImpl`. Returns the `pool_id`
    /// (`get_pool(id)` after this). `registry_addresses` is the list of
    /// Curve registry/factory contract addresses consulted for the `lp_token`
    /// / metapool lookups.
    ///
    /// `block` defaults to the current chain head.
    ///
    /// Raises:
    ///     `RuntimeError`: No `ConstructionIo` attached (requires an alloy
    ///         provider), or a builder RPC/decode failure.
    #[pyo3(signature = (address, registry_addresses, block=None))]
    fn build_curve_pool(
        &self,
        py: Python<'_>,
        address: &str,
        registry_addresses: &Bound<'_, PyList>,
        block: Option<u64>,
    ) -> PyResult<u64> {
        use degenbot_bot::bot_core::pool_builder::builder;
        use degenbot_core::runtime::get_runtime;
        let addr = parse_address(address)?;
        let registry = parse_address_list(registry_addresses)?;
        let io = self.bot.construction_io_arc().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "build_curve_pool: no ConstructionIo attached (requires an alloy provider)",
            )
        })?;
        let params = py
            .detach(|| {
                get_runtime().block_on(builder::build_curve_pool(addr, &registry, &io, block))
            })
            .map_err(map_builder_err)?;
        // Incident 2026-08-20: see unregister_pool (no GIL while parked on
        // the BotState write).
        Ok(self.with_state_mut(py, |s| s.register_curve_pool(&params)))
    }

    /// Register a Balancer V2 weighted pool (ADR-005 slice 12a state port).
    ///
    /// Stores immutable pool config (`pool_id`, vault, tokens, weights,
    /// `scaling_factors`, `swap_fee`, `pow_version`) + registration balances + a
    /// genesis reorg journal delta. The slice 12b Python `BalancerV2Pool`
    /// companion is constructed over the returned `PyLiquidityPool` handle
    /// (call `get_pool(id)` after this).
    ///
    /// Returns the auto-assigned numeric pool ID. `tokens` / `weights` /
    /// `scaling_factors` / `balances` lists MUST all have length `N`.
    ///
    /// Raises:
    ///     `ValueError`: If an address is malformed, the pool is already
    ///         registered, the list lengths mismatch, or `pool_id_hex` is not
    ///         a 32-byte hex string.
    #[expect(clippy::too_many_arguments)]
    #[pyo3(signature = (
        address,
        vault,
        pool_id_hex,
        tokens,
        weights,
        scaling_factors,
        swap_fee,
        pow_version,
        balances,
        update_block
    ))]
    fn register_balancer_weighted_pool(
        &self,
        py: Python<'_>,
        address: &str,
        vault: &str,
        pool_id_hex: &str,
        tokens: &Bound<'_, PyList>,
        weights: &Bound<'_, PyList>,
        scaling_factors: &Bound<'_, PyList>,
        swap_fee: u128,
        pow_version: u8,
        balances: &Bound<'_, PyList>,
        update_block: u64,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let vault_addr = parse_address(vault)?;
        let pool_id_bytes = hex_string_to_pool_id(pool_id_hex)?;
        let token_addrs = parse_address_list(tokens)?;
        let weight_vals = extract_u256_list(weights)?;
        let scaling_vals = extract_u256_list(scaling_factors)?;
        let bal_vals = extract_u256_list(balances)?;
        let p = RegisterBalancerWeightedPoolParams {
            address: addr,
            vault: vault_addr,
            pool_id: pool_id_bytes,
            tokens: token_addrs,
            weights: weight_vals,
            scaling_factors: scaling_vals,
            swap_fee,
            pow_version,
            balances: bal_vals,
            update_block,
        };
        // Incident 2026-08-20 #2: never hold the GIL while parked on the
        // BotState write - the dispatch fan-out's per-candidate tasks hold
        // the read end across provider fetches, and a parked GIL-writer
        // freezes every GIL consumer (main asyncio, log drainer, gil-probe).
        // Evidence: /tmp/degenbot-gil-deadlock-2026-08-20 (26 readers, state 0x1b).
        Ok(self.with_state_mut(py, |s| s.register_balancer_weighted_pool(&p)))
    }

    /// Register a Balancer V2 stable pool (ADR-005 slice 12c state port).
    ///
    /// Stores immutable pool config (`pool_id`, vault, tokens, amp,
    /// `scaling_factors`, `swap_fee`, `bpt_idx`, `invariant_version`) +
    /// registration balances + a genesis reorg journal delta. The slice 12d
    /// Python `BalancerV2StablePool` companion will be constructed over the
    /// returned `PyLiquidityPool` handle (call `get_pool(id)` after this).
    ///
    /// Returns the auto-assigned numeric pool ID. `tokens` / `scaling_factors`
    /// / `balances` lists MUST all have length `N`. `bpt_idx` is `None` for
    /// `MetaStablePools` and `Some(i)` (`i < N`) for `ComposableStablePools`.
    ///
    /// Raises:
    ///     `ValueError`: If an address is malformed, the pool is already
    ///         registered, the list lengths mismatch, `bpt_idx` is
    ///         out-of-range, or `pool_id_hex` is not a 32-byte hex string.
    #[expect(clippy::too_many_arguments)]
    #[pyo3(signature = (
        address,
        vault,
        pool_id_hex,
        tokens,
        amp,
        scaling_factors,
        swap_fee,
        bpt_idx,
        invariant_version,
        balances,
        update_block,
        rate_provider=None
    ))]
    fn register_balancer_stable_pool(
        &self,
        py: Python<'_>,
        address: &str,
        vault: &str,
        pool_id_hex: &str,
        tokens: &Bound<'_, PyList>,
        amp: u128,
        scaling_factors: &Bound<'_, PyList>,
        swap_fee: u128,
        bpt_idx: Option<usize>,
        invariant_version: u8,
        balances: &Bound<'_, PyList>,
        update_block: u64,
        rate_provider: Option<Bound<'_, PyAny>>,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let vault_addr = parse_address(vault)?;
        let pool_id_bytes = hex_string_to_pool_id(pool_id_hex)?;
        let token_addrs = parse_address_list(tokens)?;
        let scaling_vals = extract_u256_list(scaling_factors)?;
        let bal_vals = extract_u256_list(balances)?;
        let provider =
            rate_provider.map(|b| crate::bot::pool::make_balancer_rate_provider(b.unbind()));
        let params = RegisterBalancerStablePoolParams {
            address: addr,
            vault: vault_addr,
            pool_id: pool_id_bytes,
            tokens: token_addrs,
            amp,
            scaling_factors: scaling_vals,
            swap_fee,
            bpt_idx,
            invariant_version,
            balances: bal_vals,
            update_block,
            rate_provider: provider,
        };
        // Incident 2026-08-20: see unregister_pool (no GIL while parked on
        // the BotState write).
        Ok(self.with_state_mut(py, |s| s.register_balancer_stable_pool(&params)))
    }

    /// Register an Aerodrome V2 pool by contract address (ADR-005 Aerodrome
    /// state port).
    ///
    /// Stores immutable identity (address, tokens, factory, variant, stable
    /// flag, unidirectional fee) + the registration reserves + a genesis
    /// reorg-journal anchor (mirror of V2's discipline). Returns the
    /// auto-assigned pool ID. Call `get_pool(id)` after this to obtain the
    /// `PyLiquidityPool` handle.
    ///
    /// Raises:
    ///     `ValueError`: If an address is malformed, the pool is already
    ///         registered, or `variant` is not a recognized Aerodrome variant.
    #[expect(clippy::too_many_arguments)]
    #[pyo3(signature = (address, token0, token1, factory, variant, stable, fee_numer, fee_denom, token0_decimals, token1_decimals, reserve0, reserve1, update_block=0))]
    fn register_aerodrome_pool(
        &self,
        py: Python<'_>,
        address: &str,
        token0: &str,
        token1: &str,
        factory: &str,
        variant: &str,
        stable: bool,
        fee_numer: u64,
        fee_denom: u64,
        token0_decimals: u8,
        token1_decimals: u8,
        reserve0: &Bound<'_, PyAny>,
        reserve1: &Bound<'_, PyAny>,
        update_block: u64,
    ) -> PyResult<u64> {
        let addr = parse_address(address)?;
        let t0 = parse_address(token0)?;
        let t1 = parse_address(token1)?;
        let fac = parse_address(factory)?;
        let r0 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve0)?,
            "reserve0",
        )?;
        let r1 = narrow_reserve(
            crate::conversion::alloy::extract_python_u256(reserve1)?,
            "reserve1",
        )?;
        let variant_enum = DexVariant::from_kebab(variant).ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err(format!("unknown variant: {variant}"))
        })?;

        // Verify the pool address against the JSON-sourced EIP-1167 deployer
        // + implementation address (Fork A follow-on, S5SJXF/WLJD2Y — the
        // Aerodrome parity gap of JC6OFG). Skipped if (chain, factory) is
        // not in the shipped JSON or the row has no implementation address —
        // preserves the manual/ad-hoc registration path.
        crate::bot::deployments::verify_aerodrome_v2(
            self.bot.chain_id(),
            fac,
            addr,
            t0,
            t1,
            stable,
        )?;

        let p = RegisterAerodromeV2PoolParams {
            address: addr,
            token0: t0,
            token1: t1,
            factory: fac,
            variant: variant_enum,
            stable,
            fee: (fee_numer, fee_denom),
            token0_decimals,
            token1_decimals,
            reserve0: r0,
            reserve1: r1,
            update_block,
        };
        // Incident 2026-08-20 #2: never hold the GIL while parked on the
        // BotState write - the dispatch fan-out's per-candidate tasks hold
        // the read end across provider fetches, and a parked GIL-writer
        // freezes every GIL consumer (main asyncio, log drainer, gil-probe).
        // Evidence: /tmp/degenbot-gil-deadlock-2026-08-20 (26 readers, state 0x1b).
        Ok(self.with_state_mut(py, |s| s.register_aerodrome_pool(&p)))
    }

    /// Update a V3 pool's state from a Swap event.
    ///
    /// No-op if the pool is not registered.
    #[pyo3(signature = (address, sqrt_price_x96, liquidity, tick, block_number))]
    fn update_v3_pool(
        &self,
        py: Python<'_>,
        address: &str,
        sqrt_price_x96: &Bound<'_, PyAny>,
        liquidity: &Bound<'_, PyAny>,
        tick: i32,
        block_number: u64,
    ) -> PyResult<()> {
        let addr = parse_address(address)?;
        let spx = crate::conversion::alloy::extract_python_u256(sqrt_price_x96)?;
        let liq = crate::conversion::alloy::extract_python_u256(liquidity)?.to::<u128>();

        self.with_state_mut(py, |s| {
            s.update_v3_pool(addr, spx, liq, tick, block_number, vec![]);
        });
        Ok(())
    }

    /// Get the number of deltas in the reorg journal for a V3 pool.
    ///
    /// Returns 0 if the pool ID is not registered or is not a V3 pool.
    fn v3_journal_len(&self, py: Python<'_>, pool_id: u64) -> usize {
        // GIL hygiene: read guard acquired inside py.detach (inversion class).
        self.with_state(py, |s| {
            // Family guard: 0 for non-V3/V4 (preserves the per-family contract).
            if s.get_v3_or_v4_pool(pool_id).is_none() {
                0
            } else {
                s.pool_journal_len(pool_id).unwrap_or(0)
            }
        })
    }

    /// Discard V3 reorg journal deltas earlier than the given block.
    /// Discard V3 reorg journal deltas earlier than the given block.
    ///
    /// No-op if the earliest delta is at/after the target; errors if the target
    /// is past the newest delta. Raises `ValueError` on error (ADR-005 slice 4).
    #[pyo3(signature = (pool_id, block))]
    fn v3_discard_before_block(&self, py: Python<'_>, pool_id: u64, block: u64) -> PyResult<()> {
        // Incident 2026-08-20: the whole write scope runs detached - the GIL
        // is released while parked behind a live reader.
        // GIL hygiene: the write guard is acquired inside the accessor's py.detach.
        self.with_state_mut(py, |core| {
            // Family guard: no-op for non-V3/V4 (V3's contract).
            if core.get_v3_or_v4_pool(pool_id).is_none() {
                return Ok(());
            }
            core.discard_pool_before_block(pool_id, block)
                .unwrap_or(Ok(()))
                .map_err(journal_err_to_py)
        })
    }

    /// Restore V3 pool state prior to a target block.
    ///
    /// Returns `(sqrt_price_x96, liquidity, tick, block)` as Python ints,
    /// or `None` if the pool ID is not registered or not a V3 pool.
    #[pyo3(signature = (pool_id, block))]
    fn v3_restore_before_block(
        &self,
        py: Python<'_>,
        pool_id: u64,
        block: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        // Read-after-restore (ADR-016 D4): the trait `restore_pool_before_block`
        // returns `()`; the post-restore scalar fields ARE the before-values
        // the `V3RestoreResult.scalar_priors` previously carried. Copy out
        // under the write guard, then marshal after release.
        // Incident 2026-08-20: the whole write scope runs inside the
        // accessor's py.detach - the GIL is released while parked behind a
        // live reader. Owned scalars come out (the !Send guard never leaves).
        let restored = self.with_state_mut(py, |core| {
            if core.get_v3_or_v4_pool(pool_id).is_none() {
                return Ok(None);
            }
            match core.restore_pool_before_block(pool_id, block) {
                None => Ok(None),
                Some(Err(e)) => Err(journal_err_to_py(e)),
                Some(Ok(())) => {
                    #[expect(clippy::expect_used)] // invariant-guarded (documented)
                    let state = core
                        .get_v3_or_v4_pool(pool_id)
                        .expect("V3/V4 pool confirmed above");
                    Ok(Some((
                        state.sqrt_price_x96(),
                        state.liquidity(),
                        state.tick(),
                        state.update_block(),
                    )))
                }
            }
        })?;
        let Some((sqrt_p, liq, tick, blk)) = restored else {
            return Ok(None);
        };
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                crate::conversion::alloy::u256_to_py(py, &sqrt_p)?.unbind(),
                liq.into_pyobject(py)?.into_any().unbind(),
                tick.into_pyobject(py)?.into_any().unbind(),
                blk.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }

    /// Register a token.
    ///
    /// Args:
    ///     `address`: Token contract address (hex string).
    ///     `name`: Token name.
    ///     `symbol`: Token symbol.
    ///     `decimals`: Token decimals.
    ///     `chain_id`: Chain ID.
    #[pyo3(signature = (address, name, symbol, decimals, chain_id))]
    fn register_token(
        &self,
        py: Python<'_>,
        address: &str,
        name: &str,
        symbol: &str,
        decimals: u8,
        chain_id: u64,
    ) -> PyResult<PyErc20Token> {
        let addr = parse_address(address)?;
        let name = name.to_string();
        let symbol = symbol.to_string();
        // YLYJM2: release the GIL across the BotState write-lock acquisition +
        // insert so the live pump (tokio) + the asyncio loop keep making GIL
        // progress while the main thread awaits `core.write()`. Pre-fix no
        // registration seam released the GIL, so a parked `core.write()` /
        // `engine.lock()` carried the GIL with it — the startup-stall enabler
        // (the asyncio loop froze for the whole park; the pump's GIL-requiring
        // notify/log path could not progress). The closure touches no Python
        // objects: `BotState::register_token` is pure Rust insertion; the
        // `PyErc20Token` handle is built afterward from the returned `addr`.
        // Behavior-preserving (lock + insert unchanged).
        self.with_state_mut(py, |s| {
            s.register_token(addr, name, symbol, decimals, chain_id);
        });
        Ok(PyErc20Token::new(self.bot.state_arc(), addr))
    }

    /// Get a thin `PyErc20Token` handle for the given address.
    ///
    /// Args:
    ///     `address`: Token contract address (hex string).
    ///
    /// Returns:
    ///     A `PyErc20Token` handle, or `None` if the address is not registered.
    fn get_token(&self, py: Python<'_>, address: &str) -> PyResult<Option<PyErc20Token>> {
        let addr = parse_address(address)?;
        // GIL hygiene: read guard acquired inside py.detach (inversion class).
        if self.with_state(py, |s| s.has_token(&addr)) {
            Ok(Some(PyErc20Token::new(self.bot.state_arc(), addr)))
        } else {
            Ok(None)
        }
    }

    /// Build + register an ERC-20 token, resolving metadata DB-first, then
    /// on-chain (the core twin of `Erc20Builder.build`, VK3YDM-S2).
    ///
    /// `builder::build_erc20_metadata` resolves `name`/`symbol`/`decimals`
    /// (DB row → on-chain batched read → alternate-prototype fallback →
    /// UNKNOWN sentinels, with a DB write-back), then this adapter registers
    /// the token into `BotState.tokens` (ADR-006) exactly like
    /// [`Self::register_token`], and returns a thin [`PyErc20Token`] handle.
    ///
    /// Args:
    ///     `address`: token contract address (hex string).
    ///     `chain_id`: chain ID.
    ///     `block`: optional block to read on-chain at (None = head).
    #[pyo3(signature = (address, chain_id, block=None))]
    #[expect(clippy::cast_possible_wrap)] // chain_id u64 -> core i64 (ids are small)
    fn build_erc20_token(
        &self,
        py: Python<'_>,
        address: &str,
        chain_id: u64,
        block: Option<u64>,
    ) -> PyResult<PyErc20Token> {
        use degenbot_bot::bot_core::pool_builder::builder;
        use degenbot_core::runtime::get_runtime;
        let addr = parse_address(address)?;
        let io = self.bot.construction_io_arc().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "build_erc20_token: no ConstructionIo attached (requires an alloy provider)",
            )
        })?;
        let (name, symbol, decimals) = py
            .detach(|| {
                get_runtime().block_on(builder::build_erc20_metadata(
                    &io,
                    chain_id as i64,
                    addr,
                    block,
                ))
            })
            .map_err(map_builder_err)?;
        self.with_state_mut(py, |s| {
            s.register_token(addr, name, symbol, decimals, chain_id);
        });
        Ok(PyErc20Token::new(self.bot.state_arc(), addr))
    }

    /// Encode a V2 swap call, returning `(to_address_hex, calldata_hex, value)`.
    ///
    /// Args:
    ///     `pool_id`: The pool ID returned by `register_v2_pool`.
    ///     `zero_for_one`: True for token0→token1, False for token1→token0.
    ///     `amount_out`: Output token amount (Python int).
    ///     `recipient`: Address to receive output tokens (hex string).
    ///
    /// Returns:
    ///     A tuple `(to_hex, calldata_hex, value)` or `None` if pool not found.
    #[pyo3(signature = (pool_id, zero_for_one, amount_out, recipient))]
    fn encode_swap(
        &self,
        py: Python<'_>,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: &Bound<'_, PyAny>,
        recipient: &str,
    ) -> PyResult<Option<(String, String, u64)>> {
        let amount = crate::conversion::alloy::extract_python_u256(amount_out)?;
        let recip = parse_address(recipient)?;

        // GIL hygiene: read guard acquired inside py.detach (inversion class).
        let result = self.with_state(py, |s| s.encode_swap(pool_id, zero_for_one, amount, recip));

        Ok(result.map(|call| {
            let to_hex = format!("{:#x}", call.to);
            let data_hex = format!("0x{}", bytes_to_hex(&call.data));
            (to_hex, data_hex, call.value.to::<u64>())
        }))
    }

    /// Get the number of deltas in the reorg journal for a V2 pool.
    ///
    /// Returns 0 if the pool ID is not registered.
    fn v2_journal_len(&self, py: Python<'_>, pool_id: u64) -> usize {
        // GIL hygiene: read guard acquired inside py.detach (inversion class).
        self.with_state(py, |s| {
            // Family guard: 0 for non-V2 (preserves the per-family contract).
            if s.get_v2_pool_state(pool_id).is_none() {
                0
            } else {
                s.pool_journal_len(pool_id).unwrap_or(0)
            }
        })
    }

    /// Discard V2 reorg journal deltas earlier than the given block.
    ///
    /// No-op if the earliest delta is at/after the target; errors if the
    /// target is past the newest delta (would remove every known state).
    ///
    /// Raises:
    ///     `ValueError`: If the target is past the newest delta.
    #[pyo3(signature = (pool_id, block))]
    fn v2_discard_before_block(&self, py: Python<'_>, pool_id: u64, block: u64) -> PyResult<()> {
        // Incident 2026-08-20: the whole write scope runs detached - the GIL
        // is released while parked behind a live reader.
        // GIL hygiene: the write guard is acquired inside the accessor's py.detach.
        self.with_state_mut(py, |core| {
            // Family guard: no-op for non-V2 (V2's contract).
            if core.get_v2_pool_state(pool_id).is_none() {
                return Ok(());
            }
            core.discard_pool_before_block(pool_id, block)
                .unwrap_or(Ok(()))
                .map_err(journal_err_to_py)
        })
    }

    /// Restore V2 pool state prior to a target block.
    ///
    /// Pops reorg journal deltas at/after the target block and restores the
    /// landed-at state into the current fluid fields.
    ///
    /// Returns `(reserve0, reserve1, block)` as Python ints, or `None`
    /// if the pool ID is not registered.
    ///
    /// Raises:
    ///     `ValueError`: If the target is at or before the registration block
    ///         (no state exists before it) — rolling back past registration is
    ///         a hard error, not a silent no-op (ADR-005 slice 4 decision 3).
    #[pyo3(signature = (pool_id, block))]
    fn v2_restore_before_block(
        &self,
        py: Python<'_>,
        pool_id: u64,
        block: u64,
    ) -> PyResult<Option<Py<PyAny>>> {
        // Read-after-restore (ADR-016): the trait returns `()`; the
        // post-restore reserves ARE the before-values the per-family tuple
        // previously carried. Copy out under the guard, marshal after release.
        // Incident 2026-08-20: the whole write scope runs inside the
        // accessor's py.detach - the GIL is released while parked behind a
        // live reader. Owned scalars come out (the !Send guard never leaves).
        let restored = self.with_state_mut(py, |core| {
            if core.get_v2_pool_state(pool_id).is_none() {
                return Ok(None);
            }
            match core.restore_pool_before_block(pool_id, block) {
                None => Ok(None),
                Some(Err(e)) => Err(journal_err_to_py(e)),
                Some(Ok(())) => {
                    #[expect(clippy::expect_used)] // invariant-guarded (documented)
                    let state = core
                        .get_v2_pool_state(pool_id)
                        .expect("V2 pool confirmed above");
                    Ok(Some((
                        state.reserve0.to::<alloy::primitives::U256>(),
                        state.reserve1.to::<alloy::primitives::U256>(),
                        state.update_block,
                    )))
                }
            }
        })?;
        let Some((r0, r1, blk)) = restored else {
            return Ok(None);
        };
        let tuple = pyo3::types::PyTuple::new(
            py,
            [
                crate::conversion::alloy::u256_to_py(py, &r0)?.unbind(),
                crate::conversion::alloy::u256_to_py(py, &r1)?.unbind(),
                blk.into_pyobject(py)?.into_any().unbind(),
            ],
        )?;
        Ok(Some(tuple.into_any().unbind()))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_address(s: &str) -> PyResult<Address> {
    s.parse()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Invalid address '{s}': {e}")))
}

/// Build a Python `{tick: (liquidity_gross, liquidity_net, block)}` dict from
/// the helper's `HashMap<i32, TickInfo>`. Symmetric with the `tick_data` arg
/// shape on `register_v3_pool` / `register_v4_pool`, so the builder can pass
/// the returned dict straight back into `register_*_pool(tick_data=..., ...)`
/// without reshaping. `liquidity_gross` narrows `U128 → u128` (infallible for
/// valid on-chain values — Uniswap's `type(uint128).max` cap); `liquidity_net`
/// passes through `i256_to_py`; `block` is `u64` (pyo3 maps to a Python int).
fn build_tick_rows_py<'py>(
    py: Python<'py>,
    ticks: &std::collections::HashMap<i32, degenbot_bot::bot_core::TickInfo>,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for (&tick, info) in ticks {
        // `u128` / `u64` → Python int via pyo3 (arbitrary-precision — no overflow).
        let py_gross: Bound<'py, PyAny> = info
            .liquidity_gross
            .to::<u128>()
            .into_pyobject(py)?
            .into_any();
        let py_net = crate::conversion::alloy::i256_to_py(py, &info.liquidity_net)?;
        let py_block: Bound<'py, PyAny> = info.block.into_pyobject(py)?.into_any();
        let tup = pyo3::types::PyTuple::new(py, [py_gross, py_net, py_block])?;
        dict.set_item(tick, tup)?;
    }
    Ok(dict)
}

/// Parse a Python list of address strings into `Vec<Address>`.
fn parse_address_list(list: &Bound<'_, PyList>) -> PyResult<Vec<Address>> {
    list.iter()
        .map(|item| {
            let s: String = item.extract().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("token address must be a str: {e}"))
            })?;
            parse_address(&s)
        })
        .collect()
}

/// Extract a Python list of ints (or int-like) into `Vec<U256>`.
pub(crate) fn extract_u256_list(
    list: &Bound<'_, PyList>,
) -> PyResult<Vec<alloy::primitives::U256>> {
    list.iter()
        .map(|item| crate::conversion::alloy::extract_python_u256(&item))
        .collect()
}

/// Map a [`JournalError`] to a Python `ValueError` with the `NoPoolStateAvailable`
///-shaped message the Python pool companion expects (and re-raises as
/// `NoPoolStateAvailable`). ADR-005 slice 4 decision 2: reorg errors that used
/// to panic must surface as `ValueError`. Shared by `PyBot` and `PyLiquidityPool`.
pub(crate) fn journal_err_to_py(e: JournalError) -> PyErr {
    match e {
        JournalError::NoStatePriorToBlock { block } => pyo3::exceptions::PyValueError::new_err(
            format!("No pool state known prior to block {block}"),
        ),
        JournalError::NoStateAtOrAfterBlock { block } => pyo3::exceptions::PyValueError::new_err(
            format!("No pool state known at or after block {block}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]
    use super::*;

    /// Lowercase hex encode (no `0x` prefix) — for `OfflineProvider` call keys.
    fn hex_encode(bytes: &[u8]) -> String {
        const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            s.push(HEX_CHARS[(b >> 4) as usize] as char);
            s.push(HEX_CHARS[(b & 0x0f) as usize] as char);
        }
        s
    }

    /// 32-byte big-endian `uint256` word (hex, no `0x`).
    fn word_u256(v: &alloy::primitives::U256) -> String {
        let mut buf = [0u8; 32];
        v.to_be_bytes::<32>()
            .iter()
            .enumerate()
            .for_each(|(i, &b)| {
                buf[i] = b;
            });
        hex_encode(&buf)
    }

    /// 32-byte right-aligned `uint128` word (hex).
    fn word_u128(v: u128) -> String {
        let mut buf = [0u8; 32];
        buf[16..].copy_from_slice(&v.to_be_bytes());
        hex_encode(&buf)
    }

    /// 32-byte sign-extended `int128` word (hex).
    fn word_i128(v: i128) -> String {
        let mut buf = [0u8; 32];
        let be = v.to_be_bytes();
        if v < 0 {
            buf[..16].fill(0xff);
        }
        buf[16..].copy_from_slice(&be);
        hex_encode(&buf)
    }

    /// The parity fixture DB (shared with `degenbot-bot`'s
    /// `load_snapshot_from_db_populates_store_and_seed_block` test).
    fn parity_fixture() -> Option<std::path::PathBuf> {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../degenbot-db/tests/fixtures/parity.db");
        if p.exists() {
            Some(p)
        } else {
            None
        }
    }

    /// A fresh `PyBot` has `db_handle() == None` (cold-start path — no
    /// `load_snapshot_from_db` call yet). Acceptance criterion: "Cold-start
    /// path (no `load_snapshot_from_db` call) leaves `db` as None".
    #[test]
    fn cold_start_pybot_has_no_db_handle() {
        let py_bot = PyBot::new(1);
        assert!(
            py_bot.db_handle().is_none(),
            "a fresh PyBot must not hold a DegenbotDb handle (cold-start)"
        );
    }

    /// `assemble_v3_tick_map` on a fresh `PyBot` (cold-start, no DB loaded, empty
    /// store) returns `Ok(None)` — the miss path. Ac A4YUYJ: "Returns None on
    /// miss".
    #[test]
    fn assemble_v3_tick_map_cold_start_returns_none() {
        pyo3::Python::attach(|py| {
            let py_bot = PyBot::new(1);
            let result = py_bot
                .assemble_v3_tick_map(
                    py,
                    "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    0,
                    10,
                    0,
                    None,
                )
                .expect("cold-start assemble should not error");
            assert!(result.is_none(), "cold-start (no store, no Db) → miss");
        });
    }

    /// `assemble_v4_tick_map` cold-start miss — V4 twin of the V3 cold-start test.
    #[test]
    fn assemble_v4_tick_map_cold_start_returns_none() {
        pyo3::Python::attach(|py| {
            let py_bot = PyBot::new(1);
            // 32-byte V4 pool id as 0x-hex (64 hex chars + 0x prefix).
            let pool_id_py = ("0x".to_string() + &"cc".repeat(32))
                .into_pyobject(py)
                .expect("str into pyobject")
                .into_any();
            let result = py_bot
                .assemble_v4_tick_map(
                    py,
                    "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    &pool_id_py,
                    "0x7fffffffffffffffffffffffffffffffffffffff", // StateView address (dummy)
                    0,
                    10,
                    0,
                    None,
                )
                .expect("cold-start assemble should not error");
            assert!(result.is_none(), "cold-start (no store, no Db) → miss");
        });
    }

    /// NOD4PS: Chain-arm end-to-end — a cold-start `PyBot` (empty Store, no Db)
    /// with an `io=Some(PyBotIo)` backed by an `OfflineProvider`-recorded alloy
    /// provider routes through the Chain arm + returns `Some((ticks, "sparse"))`
    /// with the recorded tick liquidity. Verifies wiring: `io=Some` → Chain arm
    /// fires → `AlloyTickBootstrapRpc` choreography runs pure-Rust under
    /// `py.detach`.
    #[test]
    fn assemble_v3_tick_map_chain_arm_fires_with_offline_io() {
        use alloy::primitives::U256;
        use degenbot_rpc::offline::OfflineProvider;
        use serde_json::json;
        use std::sync::Arc;

        pyo3::Python::attach(|py| {
            // 1. Build an `OfflineProvider` recording the V3 tick-bitmap +
            //    tick-data responses for a known word + 2 active ticks.
            //    tick=10, tick_spacing=1 → word=0, bit_pos=10.
            //    Set bits 10 + 20 → bitmap = (1<<10) | (1<<20).
            let pool_addr = alloy::primitives::Address::from([0xaa; 20]);
            let bitmap = U256::from(1u128 << 10) | U256::from(1u128 << 20);
            let addr_lower = pool_addr.to_checksum(None).to_lowercase();

            let mut calls: std::collections::HashMap<String, Option<String>> =
                std::collections::HashMap::new();

            // tickBitmap(int16) → uint256
            let bitmap_calldata = degenbot_rpc::abi::encode_tick_bitmap(0);
            calls.insert(
                format!("{}:0x{}", addr_lower, hex_encode(&bitmap_calldata)),
                Some(word_u256(&bitmap)),
            );
            // ticks(int24) → (uint128 gross, int128 net)
            for &(tick, gross, net) in &[(10_i32, 1_000u128, -500i128), (20, 2_000, 750)] {
                let td_calldata = degenbot_rpc::abi::encode_tick_data(tick);
                let mut response = String::new();
                response.push_str(&word_u128(gross));
                response.push_str(&word_i128(net));
                calls.insert(
                    format!("{}:0x{}", addr_lower, hex_encode(&td_calldata)),
                    Some(response),
                );
            }

            let json_str = json!({
                "chain_id": 1u64,
                "block_number": 100u64,
                "timestamp": 0u64,
                "calls": calls,
                "code": {},
            })
            .to_string();
            let alloy = OfflineProvider::from_json_str(&json_str)
                .expect("valid offline JSON")
                .as_alloy_provider();

            // 2. Wrap the alloy provider in a `PyAlloyProvider` + `PyBotIo`.
            let pyalloy = pyo3::Bound::new(
                py,
                crate::rpc::provider::PyAlloyProvider {
                    provider: Arc::new(alloy),
                    max_blocks_per_request: 5000,
                },
            )
            .expect("PyAlloyProvider construction");
            let provider_any: pyo3::Py<pyo3::PyAny> = pyalloy.into_any().unbind();
            let io_struct = crate::bot::py_bot_io::PyBotIo::new(py, provider_any, None, None);
            let io_bound = pyo3::Bound::new(py, io_struct).expect("wrap PyBotIo as Bound");

            // 3. Cold-start PyBot + assemble_v3_tick_map with `io=Some`.
            let py_bot = PyBot::new(1);
            let assembled = py_bot
                .assemble_v3_tick_map(py, &pool_addr.to_checksum(None), 10, 1, 100, Some(io_bound))
                .expect("Chain arm must not error")
                .expect("Chain hit → Some((ticks, sparse))");
            let (ticks, coverage) = assembled;
            assert_eq!(coverage, "sparse", "Chain-arm hit → Sparse coverage");
            assert_eq!(ticks.len(), 2, "both active ticks seeded");

            let tick_10 = ticks
                .get_item(10)
                .expect("get_item Ref")
                .expect("tick 10 present");
            let (gross10_val, _net10, _block10): (u128, i128, u64) =
                tick_10.extract().expect("tick 10 as (u128,i128,u64)");
            assert_eq!(gross10_val, 1_000);

            let tick_20 = ticks
                .get_item(20)
                .expect("get_item Ref")
                .expect("tick 20 present");
            let (gross20_val, _net20, _block20): (u128, i128, u64) =
                tick_20.extract().expect("tick 20 as (u128,i128,u64)");
            assert_eq!(gross20_val, 2_000);
        });
    }

    /// snapshot via `load_snapshot_from_db`, then `assemble_v3_tick_map(addr)`
    /// must return the seeded ticks with coverage `"tracked"`.
    #[test]
    fn assemble_v3_tick_map_returns_tracked_after_snapshot_load() {
        use alloy::primitives::{address, aliases::U128, Address, I256, U256};
        use degenbot_db::discovery::V3PoolRowInput;
        use degenbot_db::{ApplyBitmapAtWord, ApplyLiquidityAtTick};
        use std::collections::HashMap;

        // Create a temp-file path for the test DB (unique via PID + counter).
        let temp_path = std::env::temp_dir().join(format!(
            "degenbot_assemble_test_{}_{:x}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        // (Scope-guarded cleanup below; assert at end.)

        // 1. Open writable, seed exchange + V3 pool + init-map + liquidity-pos.
        {
            let (db, _state) = degenbot_db::connection::DegenbotDb::open_for_writes(&temp_path)
                .expect("open_for_writes on temp");
            let factory = address!("0x1F98431c8aD98523631AE4a59f267346ea31F984");
            db.upsert_exchange(1, "uniswap_v3", factory, None).unwrap();
            let pool_address = Address::from([0xaa; 20]);
            db.upsert_v3_pools(
                1,
                "uniswap_v3",
                1,
                1_000_000,
                &[V3PoolRowInput {
                    address: pool_address,
                    token0_address: Address::from([0x11; 20]),
                    token1_address: Address::from([0x22; 20]),
                    fee: 0,
                    tick_spacing: 10,
                }],
            )
            .unwrap();
            let pool_id: i64 = db
                .lock()
                .query_row(
                    "SELECT id FROM pools WHERE address = ?1 LIMIT 1",
                    [&pool_address.to_checksum(None)],
                    |r| r.get(0),
                )
                .unwrap();
            let mut tick_data: HashMap<i32, ApplyLiquidityAtTick> = HashMap::new();
            let mut tick_bitmap: HashMap<i32, ApplyBitmapAtWord> = HashMap::new();
            tick_data.insert(
                10,
                ApplyLiquidityAtTick {
                    liquidity_gross: U128::from(1_000_000u64),
                    liquidity_net: I256::try_from(500i64).unwrap(),
                    block: 0,
                },
            );
            // bit 1 = tick 10 at spacing 10 (T3 OMDCIY intake reconciliation
            // requires the bit to match the row position).
            tick_bitmap.insert(
                0,
                ApplyBitmapAtWord {
                    bitmap: U256::from(1u64) << 1,
                    block: 0,
                },
            );
            db.upsert_v3_liquidity_positions(pool_id, &tick_data)
                .unwrap();
            db.upsert_v3_initialization_maps(pool_id, &tick_bitmap)
                .unwrap();
        }
        // The writable connection is dropped here; the file persists.

        // 2. Construct PyBot + load the snapshot from the temp file.
        pyo3::Python::attach(|py| {
            let py_bot = PyBot::new(1);
            py_bot
                .load_snapshot_from_db(&temp_path.to_string_lossy(), 1)
                .expect("snapshot load must succeed against the seeded temp DB");

            // 3. assemble against the seeded pool — expect a hit.
            let (ticks, coverage) = py_bot
                .assemble_v3_tick_map(
                    py,
                    "0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa",
                    0,
                    10,
                    0,
                    None,
                )
                .expect("assemble hit must not error")
                .expect("store hit against the seeded V3 pool must return Some");
            assert_eq!(coverage, "tracked");
            assert_eq!(ticks.len(), 1, "one tick seeded at index 10");
            let val = ticks
                .get_item(10)
                .expect("get_item must not error")
                .expect("tick 10 must be present");
            let (gross, _net, _block) = val
                .extract::<(u128, i128, u64)>()
                .expect("tick value is (gross, net, block)");
            assert_eq!(gross, 1_000_000);
        });

        // 4. Cleanup.
        let _ = std::fs::remove_file(&temp_path);
    }

    /// T3 (OMDCIY, epic OU4SYZ): a Tracked snapshot whose bitmap and tick
    /// rows disagree (bit 2 set, row only at tick 10 / bit 1) must be
    /// REJECTED at intake with a typed error (Python `ValueError`), not
    /// registered. Sparse / Chain-arm data is indeterminate and never
    /// reconciled.
    #[test]
    fn assemble_v3_tick_map_inconsistent_tracked_snapshot_rejected() {
        use alloy::primitives::{address, aliases::U128, Address, I256, U256};
        use degenbot_db::discovery::V3PoolRowInput;
        use degenbot_db::{ApplyBitmapAtWord, ApplyLiquidityAtTick};
        use std::collections::HashMap;

        let temp_path = std::env::temp_dir().join(format!(
            "degenbot_assemble_inconsistent_{}_{:x}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        {
            let (db, _state) = degenbot_db::connection::DegenbotDb::open_for_writes(&temp_path)
                .expect("open_for_writes on temp");
            let factory = address!("0x1F98431c8aD98523631AE4a59f267346ea31F984");
            db.upsert_exchange(1, "uniswap_v3", factory, None).unwrap();
            let pool_address = Address::from([0xaa; 20]);
            db.upsert_v3_pools(
                1,
                "uniswap_v3",
                1,
                1_000_000,
                &[V3PoolRowInput {
                    address: pool_address,
                    token0_address: Address::from([0x11; 20]),
                    token1_address: Address::from([0x22; 20]),
                    fee: 0,
                    tick_spacing: 10,
                }],
            )
            .unwrap();
            let pool_id: i64 = db
                .lock()
                .query_row(
                    "SELECT id FROM pools WHERE address = ?1 LIMIT 1",
                    [&pool_address.to_checksum(None)],
                    |r| r.get(0),
                )
                .unwrap();
            // Inconsistent: word 0 bit 2 (tick 20) set; the only row is tick 10.
            let mut tick_data: HashMap<i32, ApplyLiquidityAtTick> = HashMap::new();
            tick_data.insert(
                10,
                ApplyLiquidityAtTick {
                    liquidity_gross: U128::from(1_000_000u64),
                    liquidity_net: I256::try_from(500i64).unwrap(),
                    block: 0,
                },
            );
            let mut tick_bitmap: HashMap<i32, ApplyBitmapAtWord> = HashMap::new();
            tick_bitmap.insert(
                0,
                ApplyBitmapAtWord {
                    bitmap: U256::from(1u64) << 2,
                    block: 0,
                },
            );
            db.upsert_v3_liquidity_positions(pool_id, &tick_data)
                .unwrap();
            db.upsert_v3_initialization_maps(pool_id, &tick_bitmap)
                .unwrap();
        }
        pyo3::Python::attach(|py| {
            let py_bot = PyBot::new(1);
            py_bot
                .load_snapshot_from_db(&temp_path.to_string_lossy(), 1)
                .expect("snapshot load must succeed against the seeded temp DB");
            let result = py_bot.assemble_v3_tick_map(
                py,
                "0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa",
                0,
                10,
                0,
                None,
            );
            let err =
                result.expect_err("an inconsistent Tracked snapshot must be rejected at intake");
            assert!(
                err.is_instance_of::<pyo3::exceptions::PyValueError>(py),
                "the intake rejection surfaces as a ValueError"
            );
            let message = err.to_string();
            // Either conflicting position may be reported first (bitmap-side:
            // tick 10 has a bit without a row; row-side: tick 20 has a row
            // without a bit) — require a named conflict, not a specific side.
            assert!(
                message.contains("(tick 10)") || message.contains("(tick 20)"),
                "the error must name the conflicting tick: {message}"
            );
        });
        let _ = std::fs::remove_file(&temp_path);
    }

    /// Calling `load_snapshot_from_db` against a missing DB fails cleanly and
    /// leaves `db_handle() == None` (the snapshot load failed → the cached
    /// handle is NOT armed). Acceptance criterion: "On failure, `self.db`
    /// stays `None`".
    #[test]
    fn snapshot_load_failure_leaves_db_handle_none() {
        pyo3::Python::attach(|_| {
            let py_bot = PyBot::new(1);
            let result = py_bot.load_snapshot_from_db("/nonexistent/path/to/db.sqlite", 1);
            assert!(
                result.is_err(),
                "opening a nonexistent DB path must surface a PyRuntimeError"
            );
            assert!(
                py_bot.db_handle().is_none(),
                "on snapshot-load failure the cached handle must stay None"
            );
        });
    }

    /// A successful `load_snapshot_from_db` arms `db_handle()` with the
    /// read-only `DegenbotDb` used for the snapshot stream. Acceptance
    /// criterion: "After successful load, `db_handle()` returns `Some`."
    /// Skipped if the parity fixture isn't reachable from this crate layout.
    #[test]
    fn snapshot_load_success_arms_db_handle() {
        let Some(fixture) = parity_fixture() else {
            eprintln!("skipping: parity fixture not reachable");
            return;
        };
        pyo3::Python::attach(|py| {
            let py_bot = PyBot::new(8453);
            let path_str = fixture.to_string_lossy().to_string();
            py_bot
                .load_snapshot_from_db(&path_str, 8453)
                .expect("snapshot load against the parity fixture must succeed");
            let handle = py_bot
                .db_handle()
                .expect("a successful snapshot load must arm the cached DegenbotDb handle");
            // Sanity: the cached handle is usable for a read — the snapshot
            // seed block computed at load time is readable via the cached
            // connection (proving the `Mutex<Connection>` isn't dropped).
            assert!(
                py_bot.snapshot_seed_block(py).is_some(),
                "parity fixture must record a non-cold-start snapshot seed block"
            );
            // Two clones don't interfere — PyBot retains its own Arc.
            let _second = py_bot.db_handle();
            assert!(py_bot.db_handle().is_some());
            drop(handle);
        });
    }
}
