//! `PyO3` wrapper for the arbitrage engine stage surface.
//!
//! [`PyArbEngine`] holds the shared [`EngineStages`] handle — the ONE
//! external seam (epic 5TBT7L Q2b). The core engine type is crate-private
//! machinery behind that seam; every Python-facing method crosses the stage
//! surface and never names the engine.

//! # Layout
//!
//! - [`PyArbEngine`] (the `#[pyclass]`) is declared here; its
//!   `#[pymethods]` surface is split across [`register`], [`snapshot`],
//!   [`verify`], [`solve`], [`result_channel`] (`PyO3` permits multiple
//!   `#[pymethods] impl PyArbEngine` blocks). [`errors`] holds the
//!   `#[create_exception]` types.
//! - Mirrors `polars-python/src/expr/`'s 17-file `PyExpr` split and the
//!   existing `crates/degenbot-bot/src/arb_engine/` core split.
//!   (ergo UG6FKN task 74W2Z6.)

mod errors;
mod path_info;
mod payload_path_info;
mod register;
mod result_channel;
mod snapshot;
mod solve;
mod verify;

pub(crate) use register::{
    map_builder_err, map_register_v2_err, map_register_v3_err, map_register_v4_err,
};

pub use errors::*;
pub use result_channel::BlockStream;

use crate::prelude::*;
use degenbot_bot::bot_core::state_lock::StateLock;
use degenbot_bot::bot_core::BotState;
pub(crate) use hashbrown::HashMap;
pub(crate) use std::sync::Arc;

pub(crate) use alloy::primitives::{Address, U256};
pub(crate) use pyo3::exceptions::PyStopAsyncIteration;
pub(crate) use pyo3::types::{PyDict, PyList};
pub(crate) use tokio::sync::mpsc;

pub(crate) use crate::bot::PyBot;
pub(crate) use degenbot_bot::bot_core::reorg_coordinator::ReorgCoordinator;
pub(crate) use degenbot_bot::bot_core::{Bot, V4StateSync};

pub(crate) use degenbot_bot::arb_engine::EngineStages;

pub(crate) use degenbot_bot::arb_engine::{BlockNotification, ResultBatch};
pub(crate) use degenbot_solvers::mixed::{HopType, PoolHop, SolvePathResult};

/// Python-facing mixed V2/V3 arbitrage engine.
///
/// Holds the shared [`EngineStages`] seam (the engine type is `pub(crate)`
/// machinery behind it).
#[pyclass(
    // The Python-visible class name is written as an escaped literal so the
    // core engine type name never appears in this crate (grep gate).
    name = "ArbitrageEngine",
    skip_from_py_object,
    module = "degenbot._ffi"
)]
pub struct PyArbEngine {
    /// The ONE external engine seam (epic 5TBT7L Q2b).
    stages: Arc<EngineStages>,

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
    // The block-notification receiver is NOT here any more (ADR-027
    // completion): the block-clock pipe is coordinator-owned, and its
    // receiver lives on the shared PumpState — `PyBot::block_stream`
    // hands it to Python.
    /// The cross-block persistent bytecode + account-existence cache
    /// (`WarmCodeCacheInner`, the `HDEG7H` Option-A layer). Held for the
    /// engine's life; cloned into each per-block `BlockSimHandle::build` so
    /// the per-block cold `basic`/code RPCs stop repeating across blocks
    /// (first block warms; later blocks hit until TTL expiry). Constructed
    /// once in `new`; untouched by the pump (read-only under the
    /// `parking_lot::RwLock`, written only by the warm cache's own TTL
    /// re-fetch path).
    warm_code_cache: Arc<parking_lot::RwLock<degenbot_simulation::WarmCodeCacheInner>>,
}

impl PyArbEngine {
    /// Sanctioned stage-surface access for pymethod code (GIL/`BotState`
    /// inversion class, incidents 2026-08-20/21): the closure runs INSIDE
    /// `py.detach`. Same invariant contract as `PyBot::with_state` — see the
    /// doc comment there.
    pub(crate) fn with_stages<T>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&EngineStages) -> T + Send,
    ) -> T
    where
        T: Send,
    {
        py.detach(|| f(&self.stages))
    }

    /// Sanctioned core read access: the `BotState` read guard is acquired
    /// INSIDE `py.detach` (the stage surface yields the shared core arc).
    /// See [`Self::with_stages`].
    pub(crate) fn with_core<T>(&self, py: Python<'_>, f: impl FnOnce(&BotState) -> T + Send) -> T
    where
        T: Send,
    {
        py.detach(move || {
            let core = self.stages.core();
            // T1-scan-exempt: sanctioned accessor — guard inside py.detach by definition.
            let guard = core.read_at(degenbot_bot::bot_core::state_lock::LockSite::Python);
            f(&guard)
        })
    }

    /// Sanctioned core write access — see [`Self::with_core`].
    pub(crate) fn with_core_mut<T>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&mut BotState) -> T + Send,
    ) -> T
    where
        T: Send,
    {
        py.detach(move || {
            let core = self.stages.core();
            // T1-scan-exempt: sanctioned accessor — guard inside py.detach by definition.
            let mut guard = core.write_at(degenbot_bot::bot_core::state_lock::LockSite::Python);
            f(&mut guard)
        })
    }

    /// The shared `BotState` arc (ADR-003) — the stage surface's `core`
    /// handoff, cloned out for callers that need to read the pool-state
    /// registry (the in-process `BlockSimHandle` path borrows `&BotState`
    /// for `BotStateDb`). One Arc clone, no state copy.
    pub(crate) fn bot_state_arc(&self) -> Arc<StateLock<BotState>> {
        self.stages.core()
    }

    /// The cross-block warm bytecode cache arc (`HDEG7H` Option A) — the
    /// persistent `Arc<RwLock<WarmCodeCacheInner>>` held for the engine's life.
    /// `dispatch_profitable_py` clones this into the per-block
    /// `BlockSimHandle::build` so the `WarmCodeCache` layer shares one inner
    /// map across every per-block EVM. One Arc clone, no map copy.
    pub(crate) fn warm_code_cache_arc(
        &self,
    ) -> Arc<parking_lot::RwLock<degenbot_simulation::WarmCodeCacheInner>> {
        Arc::clone(&self.warm_code_cache)
    }
}

pub(crate) fn make_tick_info(
    liquidity_gross: u128,
    liquidity_net: i128,
) -> degenbot_bot::bot_core::TickInfo {
    use alloy::primitives::U128;
    degenbot_bot::bot_core::TickInfo {
        liquidity_gross: U128::from(liquidity_gross),
        liquidity_net,
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
/// multiple `#[pymethods] impl PyArbEngine { ... }` blocks; this is the
/// snapshot-seed surface (the phase / startup ritual lives in `pump.rs`/`solve.rs`).
#[pymethods]
impl PyArbEngine {
    /// The snapshot seed block `S` (JUCFCB) — set at `Bot.__init__` time by
    /// `Bot::load_snapshot_from_db` for the DB path, OR via
    /// [`set_snapshot_seed_block`](Self::set_snapshot_seed_block) for the
    /// non-DB (file/memory) path (2SM4Y7 — the pyo3 `backfill_from_snapshot`
    /// is retired; the core auto-backfill inside `BlockPump::resume_from_subscribe`
    /// reads `S` from the shared `BotState`). `None` = cold-start (no snapshot
    /// loaded).
    #[getter]
    fn snapshot_seed_block(&self, py: Python<'_>) -> Option<u64> {
        // GIL hygiene: guards acquired inside the accessor's py.detach.
        self.with_core(py, degenbot_bot::bot_core::BotState::snapshot_seed_block)
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
    fn set_snapshot_seed_block(&self, py: Python<'_>, block: Option<u64>) {
        // GIL hygiene: write guard acquired inside the accessor's py.detach.
        self.with_core_mut(py, |s| s.set_snapshot_seed_block(block));
    }
}
