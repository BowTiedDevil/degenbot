//! Bot-owned pump / lifecycle state (ADR-006 D4).
//!
//! D4 relocates the pump lifecycle (`subscribe`, `backfill_from_snapshot`,
//! `resume`) onto `PyBot`. Today these live on `PyUniswapArbEngine` and touch
//! a cluster of fields also accessed by `snapshot.rs` (phase) and `solve.rs`
//! (coordinator). Rather than move every fieldsite in one go, this module
//! defines a shared [`PumpState`] that BOTH `PyBot` and `PyUniswapArbEngine`
//! hold — so the three pump methods can move to `PyBot` (owning the pump, per
//! D4) while the engine's snapshot/solve slices keep reading the same shared
//! state through their own `Arc<PumpState>` handle.
//!
//! `PumpState` is the lifecycle layer: engine phase, the pump handle, the
//! subscribe state held between `subscribe` and `resume`, the solve coordinator
//! + reorg coordinator + shutdown flag. The pure solve core (`Arc<Mutex<UniswapEngine>>`,
//! BotState, v3/v4 snapshot stores, verify config) stays on
//! `PyUniswapArbEngine`.

use std::sync::Arc;

use degenbot_bot::bot_core::block_pump::{BlockPump, WsEvent};
use degenbot_bot::bot_core::reorg_coordinator::ReorgCoordinator;
use degenbot_bot::bot_core::solve_coordinator::SolveCoordinator;
use degenbot_bot::bot_core::{drain_sink::DrainSink, Bot};
use degenbot_bot::solvers::uniswap_engine::{EnginePhase, UniswapEngine};
use parking_lot::Mutex;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

/// Python-facing subscribe state, held between `subscribe()` and `resume()`.
pub(crate) struct PySubscribeState {
    /// The pump instance (holds `Arc<Bot>` + `Arc<dyn DrainSink>`, provider, shutdown)
    pub(crate) pump: BlockPump,
    /// First block number observed during subscribe
    pub(crate) first_block: u64,
    /// Live WS stream for the resume phase
    pub(crate) combined_stream: futures_util::stream::BoxStream<'static, WsEvent>,
}

/// Shared lifecycle state for the pump (ADR-006 D4).
///
/// Held by both `PyBot` (the D4 pump owner) and `PyUniswapArbEngine` (whose
/// snapshot/solve slices still read `phase` / `coordinator`). One allocation
/// per chain — both wrappers carry `Arc<PumpState>` to the same instance.
///
/// T3 also relocates the verify-config fields + the engine handle here so the
/// three pump methods (`subscribe`/`backfill_from_snapshot`/`resume`) — which
/// touch the engine for `process_backfill_logs`/`last_processed_block` and
/// write the verify snapshot/backfill blocks — can live entirely on
/// `PumpState` and be driven from `PyBot`. (T5 will delete `verify_on_register`
/// + the `verify_*_block` fields; T6 re-stashes the blocks on the registry.)
pub(crate) struct PumpState {
    /// Shared engine state (for `process_backfill_logs` / `last_processed_block`).
    pub(crate) engine: Arc<parking_lot::Mutex<UniswapEngine>>,
    /// The drain-point solve coordinator (ADR-006 D4, slice 6). Holds the
    /// engine as `Arc<dyn Engine>` (via `EngineHandle`) and fans drain-tick /
    /// send / finalize / reorg calls to it under a `drain_lock`.
    pub(crate) coordinator: Arc<SolveCoordinator>,
    /// The per-event reorg coordinator (ADR-006 slice 7).
    pub(crate) reorg_coordinator: Arc<ReorgCoordinator>,
    /// The per-chain `Bot` orchestrator (ADR-006 D4). `BlockPump` clones this
    /// `Arc` so its `dispatch_log` writes flow through to the engine's reads.
    pub(crate) bot: Arc<Bot>,
    /// Shutdown flag for the pump.
    pub(crate) shutdown: Arc<std::sync::atomic::AtomicBool>,
    /// Handle for the pump task (None until `subscribe`/`resume` is called).
    pub(crate) pump_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Subscribe state held between `subscribe()` and `resume()` calls.
    pub(crate) subscribe_state: Mutex<Option<PySubscribeState>>,
    /// Engine lifecycle phase (Plan 098). Enforces ordering:
    /// Created → Subscribed → `SnapshotLoaded` → Backfilled → Resumed.
    pub(crate) phase: std::sync::atomic::AtomicU8,
    /// When True, verify each V3/V4 pool's tick data against on-chain state
    /// immediately after registration (T5 deletes this — the verify gate is
    /// orphaned; T6 reseats verify at the registry drain seam).
    pub(crate) verify_on_register: std::sync::atomic::AtomicBool,
    /// Optional HTTP RPC URL for verification during registration.
    pub(crate) verify_rpc_url: Mutex<Option<String>>,
    /// Cached Alloy provider for verification RPCs.
    pub(crate) verify_provider: Mutex<Option<degenbot_rpc::provider::AlloyProvider>>,
    /// Optional `StateView` contract address for V4 verification.
    pub(crate) verify_state_view: Mutex<Option<alloy::primitives::Address>>,
    /// The snapshot block — set by `backfill_from_snapshot`.
    pub(crate) verify_snapshot_block: Mutex<Option<u64>>,
    /// The backfill block — set by `backfill_from_snapshot`.
    pub(crate) verify_backfill_block: Mutex<Option<u64>>,
}

impl PumpState {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub(crate) fn new(
        engine: Arc<parking_lot::Mutex<UniswapEngine>>,
        coordinator: Arc<SolveCoordinator>,
        reorg_coordinator: Arc<ReorgCoordinator>,
        bot: Arc<Bot>,
    ) -> Self {
        Self {
            engine,
            coordinator,
            reorg_coordinator,
            bot,
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pump_handle: Mutex::new(None),
            subscribe_state: Mutex::new(None),
            phase: std::sync::atomic::AtomicU8::new(EnginePhase::Created as u8),
            verify_on_register: std::sync::atomic::AtomicBool::new(false),
            verify_rpc_url: Mutex::new(None),
            verify_provider: Mutex::new(None),
            verify_state_view: Mutex::new(None),
            verify_snapshot_block: Mutex::new(None),
            verify_backfill_block: Mutex::new(None),
        }
    }

    /// Read the current engine lifecycle phase.
    pub(crate) fn current_phase(&self) -> EnginePhase {
        EnginePhase::from_u8(self.phase.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Advance to `phase` (no ordering check — the caller validates).
    pub(crate) fn set_phase(&self, phase: EnginePhase) {
        self.phase.store(phase as u8, std::sync::atomic::Ordering::Relaxed);
    }

    /// Subscribe to the WS `newHeads` + logs streams (ADR-006 D4 T3).
    ///
    /// This is the Bot-owned pump entry point: PyBot::subscribe delegates here.
    /// The engine's own `subscribe` (kept for the engine-only test seam) also
    /// delegates here. The body touches only `PumpState` fields (bot,
    /// coordinator, reorg_coordinator, shutdown, pump_handle, subscribe_state,
    /// phase) — no engine reference — so it lives on the shared state both
    /// wrappers reach.
    ///
    /// # Errors
    /// `PyRuntimeError` if the pump is already started/subscribed, or the WS
    /// subscribe fails.
    pub(crate) fn subscribe(&self, rpc_url: String) -> PyResult<u64> {
        let phase = self.current_phase();
        phase
            .require(EnginePhase::Created, "subscribe")
            .map_err(PyRuntimeError::new_err)?;
        if self.pump_handle.lock().is_some() {
            return Err(PyRuntimeError::new_err(
                "Cannot subscribe: pump is already started. Call stop() first.",
            ));
        }
        if self.subscribe_state.lock().is_some() {
            return Err(PyRuntimeError::new_err(
                "Cannot subscribe: already subscribed. Call resume() first.",
            ));
        }
        let bot = Arc::clone(&self.bot);
        let sink: Arc<dyn DrainSink> = self.coordinator.clone();
        let reorg_coordinator = Arc::clone(&self.reorg_coordinator);
        let shutdown = Arc::clone(&self.shutdown);
        let runtime = degenbot_core::runtime::get_runtime();
        let subscribe_result = runtime
            .block_on(async {
                BlockPump::subscribe(&rpc_url, bot, sink, reorg_coordinator, shutdown).await
            })
            .map_err(PyRuntimeError::new_err)?;
        let (pump, state) = subscribe_result;
        *self.subscribe_state.lock() = Some(PySubscribeState {
            pump,
            first_block: state.first_block,
            combined_stream: state
                .combined_stream
                .expect("subscribe() always returns a stream"),
        });
        self.set_phase(EnginePhase::Subscribed);
        Ok(state.first_block)
    }

    /// Backfill Mint/Burn/ModifyLiquidity events from the DB snapshot block to
    /// the first WS block (ADR-006 D4 T3 — relocated onto the shared pump state
    /// so PyBot::backfill_from_snapshot delegates here).
    ///
    /// # Errors
    /// `PyRuntimeError` if the phase is wrong, subscribe wasn't called, or an
    /// `eth_getLogs` request fails.
    pub(crate) fn backfill_from_snapshot(
        &self,
        rpc_url: &str,
        snapshot_block: u64,
        chunk_size: u64,
    ) -> PyResult<u64> {
        let phase = self.current_phase();
        phase
            .require(EnginePhase::SnapshotLoaded, "backfill_from_snapshot")
            .map_err(PyRuntimeError::new_err)?;
        phase
            .require_before(EnginePhase::Backfilled, "backfill_from_snapshot")
            .map_err(PyRuntimeError::new_err)?;
        let first_ws_block = {
            let state_lock = self.subscribe_state.lock();
            if let Some(s) = state_lock.as_ref() {
                s.first_block
            } else {
                return Err(PyRuntimeError::new_err(
                    "Cannot backfill: subscribe() has not been called. Call subscribe() first.",
                ));
            }
        };
        if snapshot_block == 0 {
            log::warn!("backfill_from_snapshot: snapshot_block is 0, skipping");
            return Ok(0);
        }
        if snapshot_block >= first_ws_block {
            log::info!(
                "backfill_from_snapshot: snapshot at {snapshot_block} >= WS block {first_ws_block}, nothing to backfill"
            );
            return Ok(0);
        }
        let from_block = snapshot_block + 1;
        let to_block = first_ws_block - 1;
        let total_blocks = to_block - from_block + 1;
        log::info!(
            "backfill_from_snapshot: fetching events from block {from_block} to {to_block} ({total_blocks} blocks, chunk_size={chunk_size})"
        );
        let runtime = degenbot_core::runtime::get_runtime();
        let provider = runtime
            .block_on(async {
                degenbot_rpc::provider::AlloyProvider::new(rpc_url, 3)
                    .await
                    .map_err(|e| {
                        PyRuntimeError::new_err(format!("Failed to create provider: {e}"))
                    })
            })?;
        let provider_arc = provider.provider_arc();
        let mut total_logs = 0usize;
        let mut chunk_start = from_block;
        while chunk_start <= to_block {
            let chunk_end = (chunk_start + chunk_size - 1).min(to_block);
            let filter =
                degenbot_bot::bot_core::block_pump::build_backfill_filter(chunk_start, chunk_end);
            let logs = runtime
                .block_on(async {
                    provider_arc.get_logs(&filter).await.map_err(|e| {
                        PyRuntimeError::new_err(format!(
                            "eth_getLogs failed for blocks {chunk_start}-{chunk_end}: {e}"
                        ))
                    })
                })?;
            let chunk_log_count = logs.len();
            total_logs += chunk_log_count;
            {
                let mut engine = self.engine.lock();
                engine.process_backfill_logs(&logs, chunk_end);
            }
            log::info!(
                "backfill_from_snapshot: blocks {chunk_start}-{chunk_end}: {chunk_log_count} logs applied"
            );
            chunk_start = chunk_end + 1;
        }
        log::info!(
            "backfill_from_snapshot: complete — {total_logs} total logs applied across {total_blocks} blocks"
        );
        *self.verify_snapshot_block.lock() = Some(snapshot_block);
        let backfill_block = self.engine.lock().last_processed_block().unwrap_or(to_block);
        *self.verify_backfill_block.lock() = Some(backfill_block);
        self.set_phase(EnginePhase::Backfilled);
        Ok(total_blocks)
    }

    /// Resume the pump — begin normal WS processing (ADR-006 D4 T3).
    ///
    /// # Errors
    /// `PyRuntimeError` if the phase is wrong or subscribe wasn't called.
    pub(crate) fn resume(&self) -> PyResult<()> {
        let phase = self.current_phase();
        phase
            .require(EnginePhase::SnapshotLoaded, "resume")
            .map_err(PyRuntimeError::new_err)?;
        if phase == EnginePhase::Resumed {
            return Err(PyRuntimeError::new_err(
                "Cannot resume: engine is already in Resumed phase.",
            ));
        }
        let subscribe_state = self.subscribe_state.lock().take();
        let state = subscribe_state.ok_or_else(|| {
            PyRuntimeError::new_err(
                "Cannot resume: subscribe() has not been called. Call subscribe() first.",
            )
        })?;
        let mut pump = state.pump;
        let first_block = state.first_block;
        let combined_stream = state.combined_stream;
        self.coordinator.start();
        let handle = degenbot_core::runtime::get_runtime().spawn(async move {
            let inner_state = degenbot_bot::bot_core::block_pump::SubscribeState {
                first_block,
                first_timestamp: 0,
                combined_stream: Some(combined_stream),
            };
            pump.resume_from_subscribe(inner_state).await;
        });
        *self.pump_handle.lock() = Some(handle);
        self.set_phase(EnginePhase::Resumed);
        Ok(())
    }
}