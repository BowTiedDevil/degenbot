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
use pyo3::Bound;

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

    // -- Verify config + batch verification (ADR-006 D4 T4) ------------------
    //
    // The batch verify methods read `self.engine` (BotState snapshots) + emit
    // `future_into_py` async I/O. They live here on the shared pump state so
    // PyBot::verify_liquidity_maps (the D4 owner entry point) delegates here.
    // The engine keeps thin delegating wrappers so existing
    // `engine.verify_liquidity_maps` calls keep resolving until T8 rewires them
    // to PyBot. `set_verify_on_register` is excluded (deleted in T5).

    /// Set the HTTP RPC URL used for verification (ADR-006 D4 T4).
    pub(crate) fn set_verify_rpc_url(&self, rpc_url: String) {
        let runtime = degenbot_core::runtime::get_runtime();
        match runtime.block_on(degenbot_rpc::provider::AlloyProvider::new(&rpc_url, 3)) {
            Ok(provider) => {
                *self.verify_provider.lock() = Some(provider);
            }
            Err(e) => {
                eprintln!("[warn] Failed to create verification provider: {e}");
            }
        }
        *self.verify_rpc_url.lock() = Some(rpc_url);
    }

    /// Set the `StateView` contract address for V4 verification (ADR-006 D4 T4).
    pub(crate) fn set_verify_state_view(&self, state_view_address: String) {
        let addr: alloy::primitives::Address =
            state_view_address.parse().unwrap_or(alloy::primitives::Address::ZERO);
        *self.verify_state_view.lock() = Some(addr);
    }

    /// Verify all V3 + V4 pool liquidity maps against on-chain state (ADR-006 D4 T4).
    /// Async (`future_into_py`) — never `block_on` (the 2026-06-24 deadlock).
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn verify_liquidity_maps<'py>(
        &self,
        py: Python<'py>,
        rpc_url: String,
        tick_lens_address: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let tick_lens: alloy::primitives::Address = tick_lens_address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid tick_lens address: {e}"))
        })?;
        let state_view: alloy::primitives::Address = state_view_address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid state_view address: {e}"))
        })?;
        let (v3_pools, v4_pools) = {
            let engine = self.engine.lock();
            let core = engine.core.read();
            let v3 = core.v3_pools_snapshot();
            let v4 = core.v4_pools_snapshot();
            (v3, v4)
        };
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let provider = degenbot_rpc::provider::AlloyProvider::new(&rpc_url, 3)
                .await
                .map_err(|e| {
                    PyRuntimeError::new_err(format!(
                        "verify_liquidity_maps: failed to create provider: {e}"
                    ))
                })?;
            let v3_result = degenbot_bot::bot_core::liquidity_verifier::verify_v3_pools(
                &provider,
                tick_lens,
                &v3_pools,
                block_number,
            )
            .await;
            if let Err(mismatch) = v3_result {
                return Err(PyRuntimeError::new_err(format!(
                    "Liquidity map verification FAILED: {mismatch}"
                )));
            }
            let v4_result = degenbot_bot::bot_core::liquidity_verifier::verify_v4_pools(
                &provider,
                state_view,
                &v4_pools,
                block_number,
            )
            .await;
            if let Err(mismatch) = v4_result {
                return Err(PyRuntimeError::new_err(format!(
                    "Liquidity map verification FAILED: {mismatch}"
                )));
            }
            log::info!(
                "[verify] V3 + V4 liquidity maps OK at block {}",
                block_number.unwrap_or_default()
            );
            Ok(())
        })
    }

    /// Verify V3 liquidity maps only (ADR-006 D4 T4).
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn verify_v3_liquidity_maps<'py>(
        &self,
        py: Python<'py>,
        rpc_url: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let v3_pools = {
            let engine = self.engine.lock();
            let v3 = engine.core.read().v3_pools_snapshot();
            v3
        };
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let provider = degenbot_rpc::provider::AlloyProvider::new(&rpc_url, 3)
                .await
                .map_err(|e| {
                    PyRuntimeError::new_err(format!(
                        "verify_v3_liquidity_maps: failed to create provider: {e}"
                    ))
                })?;
            let tick_lens = alloy::primitives::Address::ZERO;
            let v3_result = degenbot_bot::bot_core::liquidity_verifier::verify_v3_pools(
                &provider,
                tick_lens,
                &v3_pools,
                block_number,
            )
            .await;
            if let Err(mismatch) = v3_result {
                return Err(PyRuntimeError::new_err(format!(
                    "V3 liquidity map verification FAILED: {mismatch}"
                )));
            }
            log::info!(
                "[verify] V3 liquidity maps OK at block {}",
                block_number.unwrap_or_default()
            );
            Ok(())
        })
    }

    /// Verify V4 liquidity maps only (ADR-006 D4 T4).
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn verify_v4_liquidity_maps<'py>(
        &self,
        py: Python<'py>,
        rpc_url: String,
        state_view_address: String,
        block_number: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let state_view: alloy::primitives::Address = state_view_address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid state_view address: {e}"))
        })?;
        let v4_pools = {
            let engine = self.engine.lock();
            let v4 = engine.core.read().v4_pools_snapshot();
            v4
        };
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let provider = degenbot_rpc::provider::AlloyProvider::new(&rpc_url, 3)
                .await
                .map_err(|e| {
                    PyRuntimeError::new_err(format!(
                        "verify_v4_liquidity_maps: failed to create provider: {e}"
                    ))
                })?;
            let v4_result = degenbot_bot::bot_core::liquidity_verifier::verify_v4_pools(
                &provider,
                state_view,
                &v4_pools,
                block_number,
            )
            .await;
            if let Err(mismatch) = v4_result {
                return Err(PyRuntimeError::new_err(format!(
                    "V4 liquidity map verification FAILED: {mismatch}"
                )));
            }
            log::info!(
                "[verify] V4 liquidity maps OK at block {}",
                block_number.unwrap_or_default()
            );
            Ok(())
        })
    }
}