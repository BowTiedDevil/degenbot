//! `PyO3` wrapper for the `ArbitrageEngine`.
//!
//! [`PyArbitrageEngine`] wraps [`ArbitrageEngine`] with a `parking_lot::Mutex`
//! for safe access from the Tokio pump task. All Python-facing methods
//! acquire the lock, perform their operation, and release it.

//! # Layout
//!
//! - [`PyArbitrageEngine`] (the `#[pyclass]`) is declared here; its
//!   `#[pymethods]` surface is split across [`register`], [`snapshot`],
//!   [`verify`], [`solve`], [`result_channel`] (`PyO3` permits multiple
//!   `#[pymethods] impl PyArbitrageEngine` blocks). [`errors`] holds the
//!   `#[create_exception]` types.
//! - Mirrors `polars-python/src/expr/`'s 17-file `PyExpr` split and the
//!   existing `crates/degenbot-bot/src/solvers/arb_engine/` core split.
//!   (ergo UG6FKN task 74W2Z6.)

mod errors;
mod path_info;
mod register;
mod result_channel;
mod snapshot;
mod solve;
mod verify;

pub(crate) use register::{map_register_v2_err, map_register_v3_err, map_register_v4_err};

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
pub(crate) use degenbot_bot::bot_core::{drain_sink::DrainSink, Bot, V4StateSync};

pub(crate) use degenbot_bot::solvers::arb_engine::engine_handle::EngineHandle;

pub(crate) use degenbot_bot::solvers::arb_engine::{
    ArbitrageEngine, BlockNotification, ResultBatch,
};
pub(crate) use degenbot_solvers::mixed::{HopType, MixedPoolRef, PoolHop, SolvePathResult};

/// Python-facing mixed V2/V3 arbitrage engine.
///
/// Wraps [`ArbitrageEngine`] with a `parking_lot::Mutex` for safe access
/// from the Tokio pump task.
#[pyclass(
    name = "ArbitrageEngine",
    skip_from_py_object,
    module = "degenbot._ffi"
)]
pub struct PyArbitrageEngine {
    /// Shared engine state
    engine: Arc<parking_lot::Mutex<ArbitrageEngine>>,
    /// Retained `EngineHandle` — the ADR-006 cycle-free owner of the strong
    /// `EngineSubscriber`. `register_path`/`register_and_solve_path` draw a
    /// live `Weak` from this (see `subscriber_weak`) so `LogDispatcher::notify`
    /// routes `on_pool_state_updated` → `insert_dirty` on the live engine.
    /// A clone of this same `Arc<EngineHandle>` is the `Arc<dyn Engine>` held
    /// by `SolveCoordinator`.
    engine_handle: Arc<EngineHandle>,
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

impl PyArbitrageEngine {}

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

/// Helper to decode a hex string (e.g. "0xabcd...") to a V4 `V4PoolId` ([u8; 32]).
pub(crate) fn hex_string_to_pool_id(
    hex_str: &str,
) -> PyResult<degenbot_decoders::v4_swap_decoder::V4PoolId> {
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
/// multiple `#[pymethods] impl PyArbitrageEngine { ... }` blocks; this is the
/// snapshot-seed surface (the phase / startup ritual lives in `pump.rs`/`solve.rs`).
#[pymethods]
impl PyArbitrageEngine {
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
