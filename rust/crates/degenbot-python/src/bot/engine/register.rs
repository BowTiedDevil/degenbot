//! `PyO3` wrapper for the `UniswapEngine` — register `#[pymethods]` slice.
//!
//! Split out of the former monolithic `py_binding.rs` (ergo UG6FKN task 74W2Z6),
//! mirroring `crates/degenbot-bot/src/solvers/uniswap_engine/`'s per-concern
//! layout. `PyO3` allows multiple `#[pymethods] impl PyUniswapArbEngine { … }`
//! blocks per type, so each concern file contributes one slice.

use super::{
    mpsc, Arc, Bot, DynamicFeePoolRejectedError, EngineHandle, EngineSubscriber,
    HookedPoolRejectedError, PoolHop, PyBot, PyList, PyUniswapArbEngine, ReorgCoordinator,
    SnapshotStore, SolveCoordinator, UniswapEngine,
};
use crate::prelude::*;

#[pymethods]
impl PyUniswapArbEngine {
    #[new]
    #[pyo3(signature = (py_bot=None))]
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
            (UniswapEngine::with_core(bot.state_arc()), bot)
        } else {
            let core = Arc::new(parking_lot::RwLock::new(
                degenbot_bot::bot_core::BotState::new(),
            ));
            let bot = Arc::new(Bot::with_core(Arc::clone(&core)));
            (UniswapEngine::with_core(core), bot)
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
        let coordinator = Arc::new(SolveCoordinator::new(vec![EngineHandle::arc_dyn(
            Arc::clone(&engine),
        )]));
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
        Self {
            engine,
            pump,
            result_rx: Arc::new(parking_lot::Mutex::new(Some(result_rx))),
            block_rx: Arc::new(parking_lot::Mutex::new(Some(block_rx))),
            v3_snapshot: SnapshotStore::new(),
            v4_snapshot: SnapshotStore::new(),
        }
    }

    /// Register a V2 pool by contract address and initial reserves.
    /// Returns the assigned `pool_id` (orientation is selected at solve time
    /// via `zero_for_one`; there is no separate reverse id — ADR-006 D3).

    /// Register a mixed arbitrage path.
    ///
    /// Each entry is (`hop_type_str`, `pool_key`, `zero_for_one`) where
    /// `hop_type_str` is "V2" or "V3".
    #[pyo3(signature = (pool_refs))]
    fn register_path(&self, pool_refs: &Bound<'_, PyList>) -> PyResult<u64> {
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
        let path_id = self
            .engine
            .lock()
            .register_path(hops)
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        // ADR-006 D4: subscribe the engine to each pool_id's state updates so
        // `Bot::dispatch_log` (driven by BlockPump) dirties the engine via the
        // `EngineSubscriber` adapter. Without this, dispatched logs apply to
        // `BotState` but never mark paths dirty → no solves (the live chain was
        // severed when `apply_log` was replaced by `dispatch_log` in slice 5).
        // Duplicate pool_ids across paths are harmless (`insert_dirty` is
        // idempotent via `HashSet`).
        let subscriber = EngineSubscriber::weak_handle(&self.engine);
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
    fn register_and_solve_path(&self, pool_refs: &Bound<'_, PyList>) -> PyResult<u64> {
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
        let path_id = self
            .engine
            .lock()
            .register_and_solve_path(hops)
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        // ADR-006 D4: subscribe the engine to each pool_id (see `register_path`).
        let subscriber = EngineSubscriber::weak_handle(&self.engine);
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
        self.pump.subscribe(rpc_url)
    }

    /// Backfill Mint/Burn/ModifyLiquidity events from the last DB snapshot
    /// block to the first WS block observed during `subscribe()`.
    ///
    /// Must be called after `subscribe()`, before `resume()`. Uses
    /// `eth_getLogs` to fetch events for the gap between the DB snapshot
    /// and the live WS connection, then applies them to the V3/V4 engines
    /// via `backfill_logs()`.
    ///
    /// This ensures that when pools are registered (with `tick_data` from the
    /// DB snapshot), any liquidity changes between the snapshot block and
    /// the current chain head are reflected in the Rust engine's state.
    ///
    /// Args:
    ///     `rpc_url`: HTTP RPC endpoint for `eth_getLogs` requests
    ///     `chunk_size`: Number of blocks per `eth_getLogs` request (default 2000)
    ///
    /// Returns the number of blocks backfilled (0 if snapshot is current).
    #[pyo3(signature = (rpc_url, snapshot_block, chunk_size=2000))]
    fn backfill_from_snapshot(
        &self,
        rpc_url: &str,
        snapshot_block: u64,
        chunk_size: u64,
    ) -> PyResult<u64> {
        // ADR-006 D4 (T3): delegates to the shared `PumpState` — the
        // Bot-owned entry point. Kept on the engine for the engine-only test
        // seam; production routes through PyBot::backfill_from_snapshot.
        self.pump
            .backfill_from_snapshot(rpc_url, snapshot_block, chunk_size)
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
}

// --- V4 registration error mapping (free helper) ---
/// Map a [`RegisterV4PoolError`] to a typed Python exception (Plan 102).
///
/// - `HookedPool` → [`HookedPoolRejectedError`]
/// - `DynamicFee` → [`DynamicFeePoolRejectedError`]
/// - `AlreadyRegistered` → `PyValueError` (a wiring/programming error, not an
///   admission category)
///
/// The message text is byte-for-byte unchanged from the legacy `Err(String)`
/// formatting so `build_paths`'s classification (now `isinstance`, was
/// substring) matches the same diagnostics.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn map_register_v4_err(err: degenbot_bot::bot_core::RegisterV4PoolError) -> pyo3::PyErr {
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
        degenbot_bot::bot_core::RegisterV4PoolError::AlreadyRegistered {
            pool_manager,
            pool_id,
        } => pyo3::exceptions::PyValueError::new_err(format!(
            "V4 pool already registered: pool_manager={pool_manager}, pool_id=0x{}",
            alloy::hex::encode(pool_id),
        )),
    }
}
