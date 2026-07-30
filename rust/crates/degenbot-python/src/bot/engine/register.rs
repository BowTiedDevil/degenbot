//! `PyO3` wrapper for the `ArbitrageEngine` — register `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` (ergo UG6FKN task 74W2Z6),
//! mirroring `crates/degenbot-bot/src/solvers/arb_engine/`'s per-concern
//! layout. `PyO3` allows multiple `#[pymethods] impl PyArbitrageEngine { … }`
//! blocks per type, so each concern file contributes one slice.

use super::{
    mpsc, ArbitrageEngine, Arc, Bot, DynamicFeePoolRejectedError, EngineHandle,
    HookedPoolRejectedError, PoolHop, PyArbitrageEngine, PyBot, PyList, ReorgCoordinator,
    SolveCoordinator,
};
use crate::prelude::*;

#[pymethods]
impl PyArbitrageEngine {
    #[new]
    #[pyo3(signature = (py_bot=None))]
    #[allow(clippy::needless_pass_by_value)]
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
        let (engine, bot) = if let Some(bot) = py_bot_ref {
            let bot = bot.borrow(py).bot_arc();
            (ArbitrageEngine::with_core(bot.state_arc()), bot)
        } else {
            let core = Arc::new(parking_lot::RwLock::new(
                degenbot_bot::bot_core::BotState::new(),
            ));
            let bot = Arc::new(Bot::with_core(Arc::clone(&core)));
            (ArbitrageEngine::with_core(core), bot)
        };
        let mut engine = engine;
        engine.set_result_channel(result_tx);
        let (block_tx, block_rx) = mpsc::unbounded_channel();
        engine.set_block_channel(block_tx);
        let engine = Arc::new(parking_lot::Mutex::new(engine));
        // ADR-006 slice 6: wrap the shared engine in `EngineHandle` (an
        // `Arc<dyn Engine>` view) and build the coordinator. The coordinator
        // replaces slice 5a's `EngineDrainSink` pass-through; it fans drain-tick
        // calls to the engine under a `drain_lock` and exposes a
        // drain-consistent `last_processed_block` (Python polls block until
        // any in-flight drain completes — no Rust/Python race).
        //
        // The `EngineHandle` is retained on `Self` (not discarded into the
        // coordinator) so `register_path`/`register_and_solve_path` can draw a
        // live `Weak<dyn PoolStateSubscriber>` from it via `subscriber_weak()`.
        // This is the ADR-006 cycle-free home for the strong subscriber: it is
        // co-owned with the engine `Arc`, so the dispatcher's `Weak::upgrade`
        // succeeds until the engine actually drops (the fix for the dangling-
        // Weak bug the 2026-07-14 hotpath capture surfaced).
        let engine_handle = Arc::new(EngineHandle::new(Arc::clone(&engine)));
        let coordinator = Arc::new(SolveCoordinator::new(vec![
            Arc::clone(&engine_handle) as Arc<dyn degenbot_bot::bot_core::engine::Engine>
        ]));
        let reorg_coordinator = Arc::new(ReorgCoordinator::new(Arc::clone(&bot)));
        let pump = Arc::new(crate::bot::pump::PumpState::new(
            Arc::clone(&engine),
            Arc::clone(&coordinator),
            Arc::clone(&reorg_coordinator),
            Arc::clone(&bot),
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
            engine,
            engine_handle,
            pump,
            result_rx: Arc::new(parking_lot::Mutex::new(Some(result_rx))),
            block_rx: Arc::new(parking_lot::Mutex::new(Some(block_rx))),
            warm_code_cache,
        }
    }

    /// Register a mixed arbitrage path.
    ///
    /// Each entry is (`hop_type_str`, `pool_key`, `zero_for_one`) where
    /// `hop_type_str` is "V2" or "V3".
    #[pyo3(signature = (pool_refs))]
    fn register_path(&self, py: Python<'_>, pool_refs: &Bound<'_, PyList>) -> PyResult<u64> {
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

        let pool_ids: Vec<u64> = hops.iter().map(|h| h.pool_id).collect();
        let engine = Arc::clone(&self.engine);
        // YLYJM2: release the GIL across the engine `Mutex` acquisition +
        // `register_path` (which internally takes `core.read()`) so the live
        // pump + asyncio loop keep making GIL progress while the main thread
        // awaits `engine.lock()`. `PoolHop` is `Send`; the error maps to a
        // `PyErr` OUTSIDE the closure (GIL-held).
        let result = py.detach(move || engine.lock().register_path(hops));
        let path_id = result.map_err(pyo3::exceptions::PyValueError::new_err)?;
        // ADR-006 D4: subscribe the engine to each pool_id's state updates so
        // `Bot::dispatch_log` (driven by BlockPump) dirties the engine via the
        // `EngineSubscriber` adapter. Without this, dispatched logs apply to
        // `BotState` but never mark paths dirty → no solves (the live chain was
        // severed when `apply_log` was replaced by `dispatch_log` in slice 5).
        // Duplicate pool_ids across paths are harmless (`insert_dirty` is
        // idempotent via `HashSet`).
        //
        // The `Weak` is drawn from the retained `EngineHandle` (the
        // cycle-free strong owner) so `LogDispatcher::notify`'s `upgrade()`
        // succeeds until the engine drops — the fix for the dangling-Weak bug
        // (2026-07-14 hotpath capture: 71 notifies → 0 dirties).
        let subscriber = self.engine_handle.subscriber_weak();
        for pool_id in pool_ids {
            self.pump.bot.attach_engine(pool_id, subscriber.clone());
        }
        Ok(path_id)
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
    ) -> PyResult<u64> {
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

        let pool_ids: Vec<u64> = hops.iter().map(|h| h.pool_id).collect();
        let engine = Arc::clone(&self.engine);
        // YLYJM2: release the GIL across the engine `Mutex` acquisition +
        // `register_and_solve_path` (engine.lock() + core.read() + the single
        // eager `solve_path`). See `register_path`. `PoolHop` is `Send`; the
        // error maps to a `PyErr` OUTSIDE the closure.
        let result = py.detach(move || engine.lock().register_and_solve_path(hops));
        let path_id = result.map_err(pyo3::exceptions::PyValueError::new_err)?;
        // ADR-006 D4: subscribe the engine to each pool_id (see `register_path`).
        let subscriber = self.engine_handle.subscriber_weak();
        for pool_id in pool_ids {
            self.pump.bot.attach_engine(pool_id, subscriber.clone());
        }
        Ok(path_id)
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
    #[allow(clippy::needless_pass_by_value)]
    #[pyo3(signature = (rpc_url))]
    fn subscribe(&self, rpc_url: String) -> PyResult<u64> {
        // ADR-006 D4 (T3): delegates to the shared `PumpState::subscribe` —
        // the Bot-owned entry point. Kept on the engine for the engine-only
        // test seam; production routes through PyBot::subscribe.
        self.pump.subscribe(&rpc_url)
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
    fn resume(&self, _py: Python<'_>) -> PyResult<()> {
        // ADR-006 D4 (T3): delegates to the shared `PumpState`.
        self.pump.resume()
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
#[allow(clippy::needless_pass_by_value)]
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
#[allow(clippy::needless_pass_by_value)]
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
#[allow(clippy::needless_pass_by_value)]
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
