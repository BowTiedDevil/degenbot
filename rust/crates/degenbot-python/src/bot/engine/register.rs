//! `PyO3` wrapper for the engine stage surface — register `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` (ergo UG6FKN task 74W2Z6),
//! mirroring `crates/degenbot-bot/src/arb_engine/`'s per-concern
//! layout. `PyO3` allows multiple `#[pymethods] impl PyArbEngine { … }`
//! blocks per type, so each concern file contributes one slice.

use super::{
    mpsc, Arc, Bot, DynamicFeePoolRejectedError, EngineStages, HookedPoolRejectedError, PoolHop,
    PyArbEngine, PyBot, PyList, ReorgCoordinator,
};
use crate::prelude::*;

use degenbot_bot::bot_core::state_lock::StateLock;

#[pymethods]
impl PyArbEngine {
    #[new]
    #[pyo3(signature = (py_bot=None))]
    #[expect(clippy::needless_pass_by_value)]
    fn new(py: Python<'_>, py_bot: Option<Py<PyBot>>) -> Self {
        let py_bot_ref = py_bot.as_ref();
        let (result_tx, result_rx) = mpsc::unbounded_channel();
        // ADR-006 D1+D4: if a `PyBot` is supplied, adopt its shared
        // `Arc<RwLock<BotState>>` so the engine reads/writes the SAME core that
        // `PyBot`/`PyLiquidityPool`/`PyErc20Token` share — and clone its `Arc<Bot>`
        // so `BlockPump`'s `dispatch_log` writes flow through to the engine's
        // reads (dissolving the dual-`BotState` split —
        // `rust-owned-bot.md` §17 stale-state root cause). Without one, allocate
        // a standalone core + wrap it in a fresh `Bot` (no-pyo3 / legacy path).
        let (core, bot) = if let Some(bot) = py_bot_ref {
            let bot = bot.borrow(py).bot_arc();
            (bot.state_arc(), bot)
        } else {
            let core = Arc::new(StateLock::new(degenbot_bot::bot_core::BotState::new()));
            let bot = Arc::new(Bot::with_core(Arc::clone(&core)));
            (core, bot)
        };
        let (block_tx, block_rx) = mpsc::unbounded_channel();
        // SZJUKL seam retirement / 5TBT7L Q2b: the stage surface IS the engine
        // seam — it builds the engine internally from the shared core, so this
        // crate never names the engine type. The pump drives it through the
        // stage hooks. Python polls the engine's own cursor
        // (`last_processed_block`): stage work runs INLINE in the
        // single-writer driver, so the cursor is drain-consistent by
        // construction (no `drain_lock` to wait on).
        // The stage surface consumes the SAME epoch ledger `Bot::dispatch_log`
        // records into — one dirty-tracking mechanism (LXDY4C). The ledger
        // is injected at construction; the stage surface owns no swap.
        let stages = Arc::new(EngineStages::with_core(core, bot.active_delta()));
        stages.set_result_channel(result_tx);
        // The block-clock pipe lives on the stage surface — header ticks
        // never touch the engine's solve state (a chain fact, not engine
        // business; B2/ADR-027 lineage). The receiver lives on the shared
        // `PumpState`; `PyBot::block_stream` hands it to Python once.
        stages.set_block_channel(block_tx);
        let reorg_coordinator = Arc::new(ReorgCoordinator::new(Arc::clone(&bot)));
        let pump = Arc::new(crate::bot::pump::PumpState::new(
            Arc::clone(&stages),
            Arc::clone(&reorg_coordinator),
            Arc::clone(&bot),
            parking_lot::Mutex::new(Some(block_rx)),
        ));
        if let Some(parent) = py_bot_ref {
            parent.borrow(py).attach_pump_state(Arc::clone(&pump));
        }
        // The cross-block warm bytecode cache (`HDEG7H` Option A) — one
        // shared `Arc<RwLock<WarmCodeCacheInner>>` for the engine's life,
        // cloned into each per-block `BlockSimHandle::build`. Empty at
        // construction; warmed lazily by the first block's cold RPCs.
        let warm_code_cache = degenbot_simulation::WarmCodeCacheInner::shared_default();
        Self {
            stages,
            pump,
            result_rx: Arc::new(parking_lot::Mutex::new(Some(result_rx))),
            warm_code_cache,
        }
    }

    /// Register a mixed arbitrage path.
    ///
    /// Each entry is (`hop_type_str`, `pool_key`, `zero_for_one`) where
    /// `hop_type_str` is "V2" or "V3".
    #[pyo3(signature = (pool_refs))]
    fn register_path(
        &self,
        py: Python<'_>,
        pool_refs: &Bound<'_, PyList>,
    ) -> PyResult<(u64, bool)> {
        let mut hops = Vec::with_capacity(pool_refs.len());
        for item in pool_refs.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (pool_id, zero_for_one), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let pool_id: u64 = tuple.get_item(0)?.extract()?;
            let zero_for_one: bool = tuple.get_item(1)?.extract()?;
            hops.push(PoolHop {
                pool_id,
                zero_for_one,
            });
        }

        if hops.len() < 2 {
            let msg = format!("Need at least 2 pool refs, got {}", hops.len());
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        let stages = Arc::clone(&self.stages);
        // YLYJM2: release the GIL across the stage-surface registration
        // (which internally takes `core.read()`) so the live pump + asyncio
        // loop keep making GIL progress. `PoolHop` is `Send`; the error maps
        // to a `PyErr` OUTSIDE the closure (GIL-held).
        //
        // PRG-4: the refusal is TYPED — a full registry surfaces as the
        // benign `PathRegistryFullError` stop; everything else stays a
        // `ValueError` with the legacy message. The `created` flag derives
        // from the path-count delta inside the stage surface (dedup returns
        // the existing id without growth), so the crawl keeps its
        // new-vs-duplicate accounting without Python-side dedup state.
        let (path_id, created) = py.detach(move || {
            stages
                .register_path(hops)
                .map_err(map_path_registration_err)
        })?;
        // SZJUKL: no engine-side registration. Touched-pool dirty tracking
        // is a BYPRODUCT of log application (`Bot::dispatch_log` records the
        // block's `EpochDelta`, which `on_resolve` consumes).
        Ok((path_id, created))
    }

    /// Register a mixed arbitrage path and eagerly solve it.
    ///
    /// Unlike `register_path`, this method also resolves and solves the path
    /// immediately, appending any profitable result to the engine's results.
    /// Used when the engine is already running (after the pump has started)
    /// so that new paths are immediately available to `latest_results()`.
    #[pyo3(signature = (pool_refs))]
    fn register_and_solve_path(
        &self,
        py: Python<'_>,
        pool_refs: &Bound<'_, PyList>,
    ) -> PyResult<(u64, bool)> {
        let mut hops = Vec::with_capacity(pool_refs.len());
        for item in pool_refs.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (pool_id, zero_for_one), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let pool_id: u64 = tuple.get_item(0)?.extract()?;
            let zero_for_one: bool = tuple.get_item(1)?.extract()?;
            hops.push(PoolHop {
                pool_id,
                zero_for_one,
            });
        }

        if hops.len() < 2 {
            let msg = format!("Need at least 2 pool refs, got {}", hops.len());
            return Err(pyo3::exceptions::PyValueError::new_err(msg));
        }

        let stages = Arc::clone(&self.stages);
        // YLYJM2: release the GIL across the stage-surface registration +
        // the single eager `solve_path`. See `register_path`. `PoolHop` is
        // `Send`; the error maps to a `PyErr` OUTSIDE the closure.
        let (path_id, created) = py.detach(move || {
            stages
                .register_and_solve_path(hops)
                .map_err(map_path_registration_err)
        })?;
        // No engine-side subscription — see `register_path` (SZJUKL).
        Ok((path_id, created))
    }

    /// Subscribe phase: open WS connections and observe until first complete block.
    ///
    /// Returns the first observed block number. Python should:
    /// 1. Run backfill up to the returned block number
    /// 2. Call `resume()` to begin normal processing
    ///
    /// A "complete" block is one where both a `newHeads` notification and at
    /// least one log for the same block have been received. This guarantees
    /// the logs subscription did not miss the start of the block.
    /// No events are buffered during subscribe — the backfill is the sole
    /// authority for the gap between snapshot and WS start.
    ///
    /// Raises `RuntimeError` if the pump is already started or subscribed.
    /// PRG-4 / IRUMXD: the registered-path cap owned by the engine path
    /// registry. The Python driver sets it once at boot from the typed
    /// config value; `None` = unlimited. `None` clears any set cap
    /// (operator override).
    #[pyo3(signature = (cap=None))]
    fn set_path_cap(&self, cap: Option<u64>) {
        self.stages
            .set_path_cap(cap.map(|c| usize::try_from(c).unwrap_or(usize::MAX)));
    }

    /// PRG-4: dedup hits counted engine-side — a duplicate registration
    /// returns the existing id and never surfaces to the driver as a skip,
    /// so the `dup` telemetry needs this witness.
    #[getter]
    fn path_dedups(&self) -> u64 {
        self.stages.path_dedups()
    }

    #[expect(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url))]
    fn subscribe(&self, py: Python<'_>, rpc_url: String) -> PyResult<u64> {
        // ADR-006 D4 (T3): delegates to the shared `PumpState::subscribe` —
        // the Bot-owned entry point. Kept on the engine for the engine-only
        // test seam; production routes through PyBot::subscribe.
        self.pump.subscribe(py, &rpc_url)
    }

    /// Resume phase: begin normal pump processing.
    ///
    /// Must be called after `subscribe()`. Takes the WS stream from the
    /// subscribe phase and begins processing events on block boundaries.
    ///
    /// After calling `resume()`, the engine processes events autonomously.
    /// Python reads results via `latest_results()` and awaits new blocks
    /// via `wait_for_block()`.
    ///
    /// Raises `RuntimeError` if `subscribe()` has not been called first.
    fn resume(&self, py: Python<'_>) -> PyResult<()> {
        // ADR-006 D4 (T3): delegates to the shared `PumpState`.
        self.pump.resume(py)
    }

    /// Stop the pump and signal the Rust core to clean up (ADR-006 D4).
    ///
    /// The symmetric teardown half of `resume()`: sets the shutdown flag and
    /// aborts the spawned pump task so a Ctrl-C exits promptly — the pump loop
    /// otherwise blocks up to `BACKFILL_TIMEOUT_SECS` (60s) on a silent WS
    /// stream before re-checking its shutdown flag, and indefinitely if the WS
    /// subscription never delivers a final frame. Aborting unblocks the
    /// `combined.next().await` immediately and drops the WS subscription
    /// futures (closing the transport). Idempotent — safe from both the
    /// session teardown path and a signal handler. Delegates to the shared
    /// `PumpState`.
    fn stop(&self, _py: Python<'_>) -> PyResult<()> {
        self.pump.stop()
    }

    /// `true` once the spawned pump task has finished — cooperative timed
    /// exit (`HOTPATH_SHUTDOWN_MS`), WS stream end, or abort/panic. The
    /// Python runner's pump watchdog polls this so a completed pump triggers
    /// the ordinary graceful shutdown instead of the runner idling forever
    /// on a dead engine.
    fn pump_finished(&self) -> bool {
        self.pump.pump_finished()
    }
}

// --- Pool-registration error mapping (free helpers, F2EVV6) ---
/// Map a [`RegisterV2PoolError`] to a typed Python exception under the
/// `PoolRegistrationError` hierarchy.
///
/// - `AlreadyRegistered` → [`PoolAlreadyRegisteredError`]
/// - `SpecViolation` → [`SpecViolationError`] (the message names the
///   offending field, its value, and the bound it violates, mirroring
///   `spec_bounds::SpecViolation`'s `Display`)
///
/// These are subclasses of `PoolRegistrationError`, which is itself a
/// subclass of `ValueError`, so a broad `except ValueError:` (or
/// `except PoolRegistrationError:` to scope just admission refusals) keeps
/// working.
pub(crate) fn map_register_v2_err(err: degenbot_bot::bot_core::RegisterV2PoolError) -> pyo3::PyErr {
    use crate::bot::engine::{PoolAlreadyRegisteredError, SpecViolationError};
    match err {
        degenbot_bot::bot_core::RegisterV2PoolError::AlreadyRegistered { address } => {
            PoolAlreadyRegisteredError::new_err(format!(
                "V2 pool already registered: address={address}"
            ))
        }
        degenbot_bot::bot_core::RegisterV2PoolError::SpecViolation(v) => {
            SpecViolationError::new_err(format!("V2 pool registration failed: {v}"))
        }
    }
}

/// Map a [`RegisterV3PoolError`] to a typed Python exception under the
/// `PoolRegistrationError` hierarchy. Mirrors [`map_register_v2_err`].
pub(crate) fn map_register_v3_err(err: degenbot_bot::bot_core::RegisterV3PoolError) -> pyo3::PyErr {
    use crate::bot::engine::{PoolAlreadyRegisteredError, SpecViolationError};
    match err {
        degenbot_bot::bot_core::RegisterV3PoolError::AlreadyRegistered { address } => {
            PoolAlreadyRegisteredError::new_err(format!(
                "V3 pool already registered: address={address}"
            ))
        }
        degenbot_bot::bot_core::RegisterV3PoolError::SpecViolation(v) => {
            SpecViolationError::new_err(format!("V3 pool registration failed: {v}"))
        }
    }
}

/// Map a [`RegisterV4PoolError`] to a typed Python exception (Plan 102 +
/// F2EVV6 unified hierarchy).
///
/// - `HookedPool` → [`HookedPoolRejectedError`] (V4 amount-modifying-hook
///   admission floor — the solver's CL math assumes no hook intervention).
/// - `DynamicFee` → [`DynamicFeePoolRejectedError`] (V4 dynamic-fee
///   admission floor — the solver assumes a fixed fee).
/// - `FeeExceedsEncoderLimit` → [`HighFeePoolRejectedError`] (V4 static-fee
///   exceeds the `cmd_executor`'s 2-byte encoding field — ergo DPODAZ; the
///   fee is protocol-valid but un-encodable and unprofitable).
/// - `AlreadyRegistered` → [`PoolAlreadyRegisteredError`] (duplicate
///   `(pool_manager, pool_id)` registration — a wiring/programming error
///   surfaced at admission time, now unified with the V2/V3 twins under
///   `PoolRegistrationError`, F2EVV6).
/// - `SpecViolation` → [`SpecViolationError`] (out-of-spec
///   sqrtPriceX96/tick/fee/tickSpacing, K3IICB stop-gap upgraded to a typed
///   exception in F2EVV6).
///
/// The message text for the V4-specific variants is byte-for-byte unchanged
/// from the legacy `Err(String)` formatting so `build_paths`'s classification
/// (now `isinstance`, was substring) matches the same diagnostics.
/// Map the typed path-registration refusal (PRG-4): a full registry is the
/// benign `PathRegistryFullError` stop signal; every `Invalid` refusal
/// keeps the legacy `ValueError` with its verbatim message.
pub(crate) fn map_path_registration_err(
    err: degenbot_bot::arb_engine::lifecycle::PathRegistrationError,
) -> pyo3::PyErr {
    match err {
        degenbot_bot::arb_engine::lifecycle::PathRegistrationError::Invalid(msg) => {
            pyo3::exceptions::PyValueError::new_err(msg)
        }
        degenbot_bot::arb_engine::lifecycle::PathRegistrationError::RegistryFull {
            cap,
            registered,
        } => crate::bot::engine::PathRegistryFullError::new_err(format!(
            "registered-path cap reached ({registered}/{cap}) — the crawl must stop discovery"
        )),
    }
}

pub(crate) fn map_register_v4_err(err: degenbot_bot::bot_core::RegisterV4PoolError) -> pyo3::PyErr {
    use crate::bot::engine::{PoolAlreadyRegisteredError, SpecViolationError};
    match err {
        degenbot_bot::bot_core::RegisterV4PoolError::HookedPool { hook_flags } => {
            HookedPoolRejectedError::new_err(format!(
                "V4 pool has amount-modifying hooks (flags=0x{hook_flags:04X}, mask=0x{:04X}) — excluded from arbitrage",
                degenbot_bot::bot_core::AMOUNT_MODIFYING_HOOK_MASK
            ))
        }
        degenbot_bot::bot_core::RegisterV4PoolError::DynamicFee { fee } => {
            DynamicFeePoolRejectedError::new_err(format!(
                "V4 pool has dynamic fee (fee=0x{fee:06X}) — excluded from arbitrage"
            ))
        }
        degenbot_bot::bot_core::RegisterV4PoolError::FeeExceedsEncoderLimit { fee } => {
            crate::bot::engine::HighFeePoolRejectedError::new_err(format!(
                "V4 pool fee (fee={fee}) exceeds the cmd_executor's 2-byte encoding limit (65535) — excluded from arbitrage"
            ))
        }
        degenbot_bot::bot_core::RegisterV4PoolError::AlreadyRegistered {
            pool_manager,
            pool_id,
        } => PoolAlreadyRegisteredError::new_err(format!(
            "V4 pool already registered: pool_manager={pool_manager}, pool_id=0x{}",
            alloy::hex::encode(pool_id),
        )),
        degenbot_bot::bot_core::RegisterV4PoolError::SpecViolation(v) => {
            SpecViolationError::new_err(format!("V4 pool registration failed: {v}"))
        }
    }
}

/// Map a Rust `PoolBuilder` error (the T4 / 4GQWZ4 delegation adapter's
/// builder stage) to a Python `RuntimeError` carrying the RPC/CREATE2/spec/DB
/// failure cause. Registration-stage errors are mapped by the `map_register_v*`
/// fns above, so this covers only the pre-registration build stage.
pub(crate) fn map_builder_err(
    err: degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError,
) -> pyo3::PyErr {
    match err {
        degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError::Rpc(e) => {
            pyo3::exceptions::PyRuntimeError::new_err(format!("pool build RPC error: {e}"))
        }
        degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError::UnknownVariant {
            factory,
        } => pyo3::exceptions::PyRuntimeError::new_err(format!(
            "pool build unknown factory {factory} — no built-in DEX variant preset"
        )),
        degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError::Spec => {
            pyo3::exceptions::PyRuntimeError::new_err("pool build out-of-spec V2 reserve")
        }
        degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError::Create2 => {
            pyo3::exceptions::PyRuntimeError::new_err(
                "pool build CREATE2 address verification failed",
            )
        }
        degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError::Db(e) => {
            pyo3::exceptions::PyRuntimeError::new_err(format!("pool build DB read failed: {e}"))
        }
        degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError::Decoding { message } => {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "pool build decode failure: {message}"
            ))
        }
        degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError::MissingIdentity {
            message,
        } => pyo3::exceptions::PyValueError::new_err(format!("V4 identity incomplete: {message}")),
        degenbot_bot::bot_core::pool_builder::builder::PoolBuilderError::TickAssembly(e) => {
            pyo3::exceptions::PyValueError::new_err(format!(
                "Tracked tick map rejected at intake: {e}"
            ))
        }
    }
}
