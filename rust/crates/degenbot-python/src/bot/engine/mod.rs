//! `PyO3` wrapper for the `UniswapEngine`.
//!
//! [`PyUniswapArbEngine`] wraps [`UniswapEngine`] with a `parking_lot::Mutex`
//! for safe access from the Tokio pump task. All Python-facing methods
//! acquire the lock, perform their operation, and release it.

//! # Layout
//!
//! - [`PyUniswapArbEngine`] (the `#[pyclass]`) is declared here; its
//!   `#[pymethods]` surface is split across [`register`], [`snapshot`],
//!   [`verify`], [`solve`], [`result_channel`] (`PyO3` permits multiple
//!   `#[pymethods] impl PyUniswapArbEngine` blocks). [`errors`] holds the
//!   `#[create_exception]` types.
//! - Mirrors `polars-python/src/expr/`'s 17-file `PyExpr` split and the
//!   existing `crates/degenbot-bot/src/solvers/uniswap_engine/` core split.
//!   (ergo UG6FKN task 74W2Z6.)

mod errors;
mod register;
mod result_channel;
mod snapshot;
mod solve;
mod verify;

pub(crate) use register::map_register_v4_err;

pub use errors::*;
pub use result_channel::BlockStream;

use crate::prelude::*;
pub(crate) use std::collections::HashMap;
pub(crate) use std::sync::Arc;

pub(crate) use alloy::primitives::{Address, U256};
pub(crate) use pyo3::exceptions::PyStopAsyncIteration;
pub(crate) use pyo3::types::{PyDict, PyList};
pub(crate) use tokio::sync::mpsc;

pub(crate) use crate::bot::PyBot;
pub(crate) use degenbot_bot::bot_core::reorg_coordinator::ReorgCoordinator;
pub(crate) use degenbot_bot::bot_core::solve_coordinator::SolveCoordinator;
pub(crate) use degenbot_bot::bot_core::{
    drain_sink::DrainSink, Bot, V3SwapUpdate, V4StateSync, V4SwapUpdate,
};

pub(crate) use degenbot_bot::solvers::uniswap_engine::engine_handle::EngineHandle;
pub(crate) use degenbot_bot::solvers::uniswap_engine::engine_subscriber::EngineSubscriber;

pub(crate) use degenbot_bot::solvers::uniswap_engine::{
    BlockMetadata, BlockNotification, EnginePhase, HopType, MixedPoolRef, PoolHop, ResultBatch,
    SolvePathResult, UniswapEngine,
};

/// Python-facing mixed V2/V3 arbitrage engine.
///
/// Wraps [`UniswapEngine`] with a `parking_lot::Mutex` for safe access
/// from the Tokio pump task.
#[pyclass(name = "UniswapArbEngine", skip_from_py_object)]
pub struct PyUniswapArbEngine {
    /// Shared engine state
    engine: Arc<parking_lot::Mutex<UniswapEngine>>,
    /// ADR-006 D4 (T3): the pump lifecycle state (coordinator, reorg
    /// coordinator, bot, shutdown, pump handle, subscribe state, phase) now
    /// lives in a shared `Arc<PumpState>` co-owned with `PyBot`. The legacy
    /// individual fields (`coordinator`, `reorg_coordinator`, `bot`, `shutdown`,
    /// `pump_handle`, `subscribe_state`, `phase`) are reachable through this
    /// handle — so snapshot.rs/solve.rs keep reading the SAME state while the
    /// three pump methods also move onto `PyBot`.
    pump: Arc<crate::bot::pump::PumpState>,
    /// Receiver for the result batch channel.
    /// Created in `new()`, consumed by `__anext__`.
    /// Wrapped in Arc so the async coroutine can share it.
    result_rx: Arc<parking_lot::Mutex<Option<mpsc::UnboundedReceiver<ResultBatch>>>>,
    /// Receiver for the block-notification channel (epic 6W35AI). The pump
    /// forwards `newHeads` ticks here via `DrainSink::notify_block`;
    /// Python consumes this as its block clock (not `ResultBatch::solve_block`).
    /// Consumed by `BlockStream::__anext__`; wrapped in Arc for the coroutine.
    block_rx: Arc<parking_lot::Mutex<Option<mpsc::UnboundedReceiver<BlockNotification>>>>,
}

impl PyUniswapArbEngine {
    /// Parse V2 Sync updates from a Python list of 3-tuples.
    pub(crate) fn parse_v2_updates(
        v2_sync_updates: &Bound<'_, PyList>,
    ) -> PyResult<Vec<(Address, U256, U256)>> {
        let mut rust_v2: Vec<(Address, U256, U256)> = Vec::with_capacity(v2_sync_updates.len());
        for item in v2_sync_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 3 {
                let msg = format!(
                    "Expected 3-tuple (address, reserve0, reserve1), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let addr_obj = tuple.get_item(0)?;
            let addr_str: String = addr_obj.extract()?;
            let addr = addr_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
            })?;
            let r0 = crate::conversion::alloy::extract_python_u256(&tuple.get_item(1)?)?;
            let r1 = crate::conversion::alloy::extract_python_u256(&tuple.get_item(2)?)?;
            rust_v2.push((addr, r0, r1));
        }
        Ok(rust_v2)
    }

    /// Parse tick priors from a Python list of 2-tuples.
    pub(crate) fn parse_tick_priors(
        priors_list: &Bound<'_, PyList>,
    ) -> PyResult<Vec<(i32, degenbot_bot::bot_core::TickInfo)>> {
        let mut tick_priors = Vec::new();
        for prior_item in priors_list.iter() {
            let prior_tuple = prior_item.cast::<pyo3::types::PyTuple>()?;
            if prior_tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (tick_index, (lg, ln)), got {} elements",
                    prior_tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let tick_idx: i32 = prior_tuple.get_item(0)?.extract()?;
            let info_obj = prior_tuple.get_item(1)?;
            let info_tuple = info_obj.cast::<pyo3::types::PyTuple>()?;
            if info_tuple.len() != 2 {
                let msg = format!(
                    "Expected 2-tuple (liquidity_gross, liquidity_net), got {} elements",
                    info_tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }
            let lg: u128 = info_tuple.get_item(0)?.extract()?;
            let ln: i128 = info_tuple.get_item(1)?.extract()?;
            tick_priors.push((tick_idx, make_tick_info(lg, ln)));
        }
        Ok(tick_priors)
    }

    /// Parse V3 Swap updates from a Python list of 5-tuples.
    pub(crate) fn parse_v3_updates(
        v3_swap_updates: &Bound<'_, PyList>,
    ) -> PyResult<Vec<V3SwapUpdate>> {
        let mut rust_v3: Vec<V3SwapUpdate> = Vec::with_capacity(v3_swap_updates.len());
        for item in v3_swap_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 5 {
                let msg = format!(
                    "Expected 5-tuple (address, sqrt_price, liquidity, tick, tick_priors), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let addr_obj = tuple.get_item(0)?;
            let addr_str: String = addr_obj.extract()?;
            let addr = addr_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {e}"))
            })?;
            let sqrt_price = crate::conversion::alloy::extract_python_u256(&tuple.get_item(1)?)?;
            let liquidity: u128 = tuple.get_item(2)?.extract()?;
            let tick: i32 = tuple.get_item(3)?.extract()?;

            let priors_obj = tuple.get_item(4)?;
            let priors_list = priors_obj.cast::<PyList>()?;
            let tick_priors = Self::parse_tick_priors(priors_list)?;

            rust_v3.push(V3SwapUpdate {
                pool_address: addr,
                sqrt_price_x96: sqrt_price,
                liquidity,
                tick,
                tick_priors,
            });
        }
        Ok(rust_v3)
    }

    /// Parse V4 Swap updates from a Python list of 6-tuples.
    pub(crate) fn parse_v4_updates(
        v4_swap_updates: &Bound<'_, PyList>,
    ) -> PyResult<Vec<V4SwapUpdate>> {
        let mut rust_v4: Vec<V4SwapUpdate> = Vec::with_capacity(v4_swap_updates.len());
        for item in v4_swap_updates.iter() {
            let tuple = item.cast::<pyo3::types::PyTuple>()?;
            if tuple.len() != 6 {
                let msg = format!(
                    "Expected 6-tuple (pool_manager, pool_id_hex, sqrt_price, liquidity, tick, tick_priors), got {} elements",
                    tuple.len()
                );
                return Err(pyo3::exceptions::PyValueError::new_err(msg));
            }

            let pm_obj = tuple.get_item(0)?;
            let pm_str: String = pm_obj.extract()?;
            let pool_manager = pm_str.parse::<Address>().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "Invalid pool_manager address: {e}"
                ))
            })?;

            let pid_obj = tuple.get_item(1)?;
            let pid_str: String = pid_obj.extract()?;
            let pool_id = hex_string_to_pool_id(&pid_str)?;

            let sqrt_price = crate::conversion::alloy::extract_python_u256(&tuple.get_item(2)?)?;
            let liquidity: u128 = tuple.get_item(3)?.extract()?;
            let tick: i32 = tuple.get_item(4)?.extract()?;

            let priors_obj = tuple.get_item(5)?;
            let priors_list = priors_obj.cast::<PyList>()?;
            let tick_priors = Self::parse_tick_priors(priors_list)?;

            rust_v4.push(V4SwapUpdate {
                pool_manager,
                pool_id,
                sqrt_price_x96: sqrt_price,
                liquidity,
                tick,
                tick_priors,
            });
        }
        Ok(rust_v4)
    }

    /// Get the current engine phase.
    pub(crate) fn current_phase(&self) -> EnginePhase {
        self.pump.current_phase()
    }

    /// Set the engine phase (advancing only).
    pub(crate) fn set_phase(&self, phase: EnginePhase) {
        self.pump.set_phase(phase);
    }
}

pub(crate) fn make_tick_info(
    liquidity_gross: u128,
    liquidity_net: i128,
) -> degenbot_bot::bot_core::TickInfo {
    use alloy::primitives::{I256, U128};
    degenbot_bot::bot_core::TickInfo {
        liquidity_gross: U128::from(liquidity_gross),
        liquidity_net: I256::try_from(liquidity_net).unwrap_or(I256::ZERO),
        block: 0,
    }
}

/// Helper to decode a hex string (e.g. "0xabcd...") to a V4 `PoolId` ([u8; 32]).
pub(crate) fn hex_string_to_pool_id(
    hex_str: &str,
) -> PyResult<degenbot_decoders::v4_swap_decoder::PoolId> {
    let hex_str = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    if hex_str.len() != 64 {
        let msg = format!(
            "Pool ID hex string must be 64 hex chars (32 bytes), got {}",
            hex_str.len()
        );
        return Err(pyo3::exceptions::PyValueError::new_err(msg));
    }
    let mut pool_id = [0u8; 32];
    for i in 0..32 {
        let byte_str = &hex_str[i * 2..i * 2 + 2];
        pool_id[i] = u8::from_str_radix(byte_str, 16).map_err(|e| {
            let msg = format!("Invalid hex in pool_id at byte {i}: {e}");
            pyo3::exceptions::PyValueError::new_err(msg)
        })?;
    }
    Ok(pool_id)
}

/// `#[pymethods]` slice for the JUCFCB snapshot-seed getter. `PyO3` allows
/// multiple `#[pymethods] impl PyUniswapArbEngine { ... }` blocks; this is the
/// snapshot-seed surface (the phase / startup ritual lives in `pump.rs`/`solve.rs`).
#[pymethods]
impl PyUniswapArbEngine {
    /// The snapshot seed block `S` (JUCFCB) — set at `Bot.__init__` time by
    /// `Bot::load_snapshot_from_db` for the DB path, OR via
    /// [`set_snapshot_seed_block`](Self::set_snapshot_seed_block) for the
    /// non-DB (file/memory) path (2SM4Y7 — the pyo3 `backfill_from_snapshot`
    /// is retired; the core auto-backfill inside `BlockPump::resume_from_subscribe`
    /// reads `S` from the shared `BotState`). `None` = cold-start (no snapshot
    /// loaded).
    #[getter]
    fn snapshot_seed_block(&self) -> Option<u64> {
        self.engine.lock().core.read().snapshot_seed_block()
    }

    /// Set the snapshot seed block `S` on the shared `BotState` for the
    /// non-DB (file/memory) snapshot path (2SM4Y7).
    ///
    /// The DB path (`Bot::load_snapshot_from_db`) sets `S` itself; the
    /// non-DB path calls this once after `load_v3_snapshot_from_py` /
    /// `load_v4_snapshot_from_py` so the shared `BotState` carries `S =
    /// min(newest_block_v3, newest_block_v4)` — the seed the core
    /// auto-backfill (J3FMDO) closes the snapshot→WS gap from.
    ///
    /// `None` clears the seed (cold-start resume); `Some(b)` overrides the
    /// stored seed (used only when no snapshot has set it yet — the DB path's
    /// already-set seed takes precedence on the production path because the
    /// non-DB path does not call this setter).
    #[setter]
    fn set_snapshot_seed_block(&self, block: Option<u64>) {
        self.engine
            .lock()
            .core
            .write()
            .set_snapshot_seed_block(block);
    }
}
