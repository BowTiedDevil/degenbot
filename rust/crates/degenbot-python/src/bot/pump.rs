//! Bot-owned pump / lifecycle state (ADR-006 D4).
//!
//! D4 relocates the pump lifecycle (`subscribe`, `backfill_from_snapshot`,
//! `resume`) onto `PyBot`. Today these live on `PyArbitrageEngine` and touch
//! a cluster of fields also accessed by `snapshot.rs` (phase) and `solve.rs`
//! (coordinator). Rather than move every fieldsite in one go, this module
//! defines a shared [`PumpState`] that BOTH `PyBot` and `PyArbitrageEngine`
//! hold — so the three pump methods can move to `PyBot` (owning the pump, per
//! D4) while the engine's snapshot/solve slices keep reading the same shared
//! state through their own `Arc<PumpState>` handle.
//!
//! `PumpState` is the lifecycle layer: engine phase, the pump handle, the
//! subscribe state held between `subscribe` and `resume`, the solve coordinator
//! + reorg coordinator + shutdown flag. The pure solve core (`Arc<Mutex<ArbitrageEngine>>`,
//!   `BotState`, v3/v4 snapshot stores, verify config) stays on
//!   `PyArbitrageEngine`.

use std::sync::Arc;

use degenbot_bot::bot_core::block_pump::{BlockPump, WsEvent};
use degenbot_bot::bot_core::reorg_coordinator::ReorgCoordinator;
use degenbot_bot::bot_core::solve_coordinator::SolveCoordinator;
use degenbot_bot::bot_core::{drain_sink::DrainSink, Bot};
use degenbot_bot::solvers::arb_engine::{ArbitrageEngine, EnginePhase};
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
/// Held by both `PyBot` (the D4 pump owner) and `PyArbitrageEngine` (whose
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
    pub(crate) engine: Arc<parking_lot::Mutex<ArbitrageEngine>>,
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
    /// When True, verify each V3/V4 pool's tick data against on-chain state
    /// immediately after registration (T5 deletes this — the verify gate moves
    /// to the registry drain seam in T6).
    pub(crate) verify_rpc_url: Mutex<Option<String>>,
    /// Cached Alloy provider for verification RPCs.
    pub(crate) verify_provider: Mutex<Option<degenbot_rpc::provider::AlloyProvider>>,
    /// Optional `StateView` contract address for V4 verification.
    pub(crate) verify_state_view: Mutex<Option<alloy::primitives::Address>>,
}

impl PumpState {
    #[must_use]
    pub(crate) fn new(
        engine: Arc<parking_lot::Mutex<ArbitrageEngine>>,
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
            verify_rpc_url: Mutex::new(None),
            verify_provider: Mutex::new(None),
            verify_state_view: Mutex::new(None),
        }
    }

    /// Read the current engine lifecycle phase (delegates to the core
    /// `ArbitrageEngine` source of truth — ZU7RAF).
    pub(crate) fn current_phase(&self) -> EnginePhase {
        self.engine.lock().current_phase()
    }

    /// Advance to `phase` (no ordering check — the caller validates).
    /// Delegates to the core `ArbitrageEngine` (ZU7RAF).
    pub(crate) fn set_phase(&self, phase: EnginePhase) {
        self.engine.lock().set_phase(phase);
    }

    /// Subscribe to the WS `newHeads` + logs streams (ADR-006 D4 T3).
    ///
    /// This is the Bot-owned pump entry point: `PyBot::subscribe` delegates here.
    /// The engine's own `subscribe` (kept for the engine-only test seam) also
    /// delegates here. The body touches only `PumpState` fields (bot,
    /// coordinator, `reorg_coordinator`, shutdown, `pump_handle`, `subscribe_state`,
    /// phase) — no engine reference — so it lives on the shared state both
    /// wrappers reach.
    ///
    /// # Errors
    /// `PyRuntimeError` if the pump is already started/subscribed, or the WS
    /// subscribe fails.
    #[tracing::instrument(skip(self, py), fields(rpc_url = %rpc_url))]
    pub(crate) fn subscribe(&self, py: Python<'_>, rpc_url: &str) -> PyResult<u64> {
        let phase = self.current_phase();
        phase
            .allow_subscribe("subscribe")
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
        // GIL-release across the WS handshake `block_on`: the handshake future
        // (BlockPump::subscribe -> observe_complete_block — pure Rust async:
        // WS subscribe + header polling) does NOT need the GIL to complete, so
        // `py.detach` is safe (no re-entry deadlock). Without it the calling
        // (asyncio main) thread holds the GIL for the whole handshake (~12s,
        // one block time) — the `gil-probe` recorded a 16.6s `GIL held`
        // acquire here. PyO3 0.29 renamed `allow_threads` to `detach`.
        let subscribe_result = py
            .detach(|| {
                runtime.block_on(async {
                    BlockPump::subscribe(rpc_url, bot, sink, reorg_coordinator, shutdown).await
                })
            })
            .map_err(PyRuntimeError::new_err)?;
        let (pump, state) = subscribe_result;
        #[expect(clippy::expect_used)] // subscribe() guarantees a stream (documented)
        {
            *self.subscribe_state.lock() = Some(PySubscribeState {
                pump,
                first_block: state.first_block,
                combined_stream: state
                    .combined_stream
                    .expect("subscribe() always returns a stream"),
            });
        }
        // J3FMDO regression fix: reflect whether the core already has a
        // snapshot loaded (the construction-time-load path —
        // `Bot::load_snapshot_from_db` at `Bot` construction writes the
        // snapshot into the shared core `BotState` but never advances the
        // engine phase). An unconditional `set_phase(Subscribed)` left the
        // phase at `Subscribed` (1) and `resume()`'s `require(SnapshotLoaded)`
        // guard crashed the production settlement-arbitrage bot:
        //   RuntimeError: Cannot call resume: engine is in phase Subscribed,
        //                 but requires SnapshotLoaded
        // `after_subscribe` lands at `SnapshotLoaded` when the core has a
        // snapshot (so `resume()` is reachable) and `Subscribed` otherwise
        // (the legacy path that loads the snapshot after subscribe).
        let core_has_snapshot = self.bot.state_arc().read().snapshot_seed_block().is_some();
        self.set_phase(EnginePhase::after_subscribe(phase, core_has_snapshot));
        Ok(state.first_block)
    }

    /// Resume the pump — begin normal WS processing (ADR-006 D4 T3).
    ///
    /// # Errors
    /// `PyRuntimeError` if the phase is wrong or subscribe wasn't called.
    #[tracing::instrument(skip(self, py))]
    pub(crate) fn resume(&self, py: Python<'_>) -> PyResult<()> {
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
        // J3FMDO race fix: run the snapshot→WS backfill SYNCHRONOUSLY before
        // spawning the live loop, so Python's `build_paths` (which drains the
        // per-pool backfill buffer via `apply_backfill_buffer_v3`) cannot race
        // the backfill. Pre-fix the backfill ran inside the spawned
        // `resume_from_subscribe` task and `resume` returned immediately — an
        // active pool's burn was not yet buffered when `build_paths` drained,
        // so the post-drain verify mismatched on-chain and crashed the settlement-arbitrage
        // bot (`VerificationMismatchError`, 2026-07-12). `block_on` on the
        // shared runtime mirrors `subscribe`'s sync discipline.
        //
        // GIL-release across the backfill `block_on`: `backfill_to_ws_block`
        // -> `backfill_from_snapshot` -> `process_backfill_logs` is pure Rust
        // async (eth_getLogs RPC + BotState mutation — no `Python::attach`,
        // no `sink.notify_block` which fires only in `run_with_stream`). It
        // does NOT need the GIL to complete, so `py.detach` is safe (no
        // re-entry deadlock). Without it the asyncio main thread holds the GIL
        // for the whole backfill (~168k logs) — the `gil-probe` recorded 8.4s
        // + 5.0s `GIL held` acquires here. J3FMDO invariant preserved: `block_on`
        // still awaits the backfill synchronously before `resume` returns; only
        // the GIL is released during the wait. PyO3 0.29 renamed `allow_threads`
        // to `detach`.
        let pump_ref = &pump;
        py.detach(|| {
            degenbot_core::runtime::get_runtime().block_on(async {
                if let Err(e) = pump_ref.backfill_to_ws_block(first_block).await {
                    tracing::error!(
                        first_block,
                        %e,
                        "BlockPump: auto-backfill failed — starting live loop with gap"
                    );
                }
            });
        });
        let handle = degenbot_core::runtime::get_runtime().spawn(async move {
            pump.run_with_stream(combined_stream, first_block).await;
        });
        *self.pump_handle.lock() = Some(handle);
        self.set_phase(EnginePhase::Resumed);
        Ok(())
    }

    /// Stop the pump and signal the Rust core to clean up (ADR-006 D4).
    ///
    /// The cooperative path (`subscribe` → `backfill_from_snapshot` → `resume`)
    /// has no symmetric teardown — a `keyboard interrupt` in Python leaves the
    /// `BlockPump` task blocking on the WS stream, so process exit blocks for
    /// up to `BACKFILL_TIMEOUT_SECS` (60s) of idle before the loop re-checks
    /// its `shutdown` flag (and indefinitely if the WS subscription never
    /// delivers a final frame). `stop()` closes that gap: it sets the flag so
    /// any in-flight loop visit notices, *then* aborts the spawned task so the
    /// `combined.next().await` unblocks immediately — dropping the WS
    /// subscription futures (which closes the transport) and all pump-held
    /// resources before returning. Idempotent — dropping the `pump_handle`
    /// (taken on the first call) makes a second call a no-op `Ok(())`.
    ///
    /// Aborting mid-`on_drain`/`on_send` is safe: those acquire internal
    /// `parking_lot` locks (non-poisoning) which release on cancellation, and
    /// `shutdown` is already `true` so no further drain tick matters. Mirror of
    /// the legacy `PyV2ArbEngine::stop()` (sets the shutdown flag and aborts
    /// the pump task).
    ///
    /// # Errors
    /// Currently always returns `Ok(())` — the abort + `JoinHandle` await
    /// never fails in a way the caller can recover from. Typed `PyResult` keeps
    /// the surface symmetric with `subscribe`/`resume` and leaves room for a
    /// future timed-join error.
    #[expect(clippy::unnecessary_wraps)]
    pub(crate) fn stop(&self) -> PyResult<()> {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let handle = self.pump_handle.lock().take();
        if let Some(handle) = handle {
            handle.abort();
            // Drive the cancelled task to completion so its held resources
            // (WS subscription futures, `Arc<dyn DrainSink>` clones) drop
            // before Python tears the runtime down. `block_on` on the shared
            // runtime matches the existing `subscribe`/`backfill_from_snapshot`
            // sync discipline; the aborted task completes promptly.
            let _ = degenbot_core::runtime::get_runtime().block_on(handle);
            tracing::info!("[shutdown] BlockPump task aborted");
        } else {
            tracing::info!("[shutdown] BlockPump not running (no pump handle to abort)");
        }
        // Drop half-built subscribe state so a later `subscribe()` is allowed
        // (the phase guard + the `subscribe_state.is_some()` check would
        // otherwise reject it).
        *self.subscribe_state.lock() = None;
        Ok(())
    }

    // -- Verify config (ADR-006 D4 T4) --------------------------------------
    //
    // The whole-batch liquidity-map verifier (`verify_liquidity_maps` and its
    // V3/V4 twins) was REMOVED as redundant + racy (the per-pool two-step
    // registration lifecycle below is the verify authority; see
    // dev-run-solver-state-findings.md Addendum 3). What remains here is only
    // the verify CONFIG the per-pool lifecycle consumes.

    /// Set the HTTP RPC URL used for verification (ADR-006 D4 T4).
    pub(crate) fn set_verify_rpc_url(&self, rpc_url: &str) {
        let runtime = degenbot_core::runtime::get_runtime();
        match runtime.block_on(degenbot_rpc::provider::AlloyProvider::new(
            rpc_url,
            degenbot_rpc::provider::DEFAULT_MAX_RETRIES,
        )) {
            Ok(provider) => {
                *self.verify_provider.lock() = Some(provider);
            }
            Err(e) => {
                #[expect(clippy::print_stderr)] // startup diagnostic
                {
                    eprintln!("[warn] Failed to create verification provider: {e}");
                }
            }
        }
        *self.verify_rpc_url.lock() = Some(rpc_url.to_string());
    }

    /// Set the `StateView` contract address for V4 verification (ADR-006 D4 T4).
    pub(crate) fn set_verify_state_view(&self, state_view_address: &str) {
        let addr: alloy::primitives::Address = state_view_address
            .parse()
            .unwrap_or(alloy::primitives::Address::ZERO);
        *self.verify_state_view.lock() = Some(addr);
    }

    /// Run a single V3 pool's registration verify-lifecycle end-to-end — the
    /// core-owned `quarantine → seed-verify → drain+pin → post-drain-verify →
    /// set_live` choreography (ADR-022 D1) that the Python driver previously
    /// performed as separate `set_*_quarantined` + `verify_*_snapshot_seed` +
    /// `apply_buffer` + `verify_*_post_drain` + `set_*_live` round-trips.
    ///
    /// **Sparse** → immediate no-op (`Live`, no RPC). **Tracked** → verified
    /// with the mismatch tripwire before `Live` (never released unverified).
    /// Uses the bot's single verify provider (D-B — one provider per
    /// bot/chain); a missing provider fails fast (D-C no-config posture).
    ///
    /// # Errors
    ///
    /// `VerificationMismatchError` (fatal tripwire) / `VerificationRpcError`
    /// (transient) from the underlying verify, or `VerificationRpcError` when
    /// no verify provider was configured.
    #[expect(clippy::needless_pass_by_value)]
    pub(crate) fn run_v3_registration_lifecycle<'py>(
        &self,
        py: Python<'py>,
        address: String,
        snapshot_block: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let pool_addr: alloy::primitives::Address = address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid V3 address: {e}"))
        })?;
        // Clone the single provider + the core handle under short locks; the
        // lifecycle re-acquires core.write() per step (no guard across await).
        // The provider is `Option`: it is resolved lazily inside the core, so
        // a missing provider only fails fast (D-C) for a TRACKED pool that
        // actually reaches a verify step — Sparse / unregistered no-op paths
        // (and the sparse buffer drain) never need one.
        let core = {
            let engine = self.engine.lock();
            Arc::clone(engine.core())
        };
        let provider = self.verify_provider.lock().clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            use degenbot_bot::bot_core::registration_lifecycle::RegistrationLifecycleError;
            degenbot_bot::bot_core::run_v3_registration_lifecycle(
                &core,
                provider.as_ref(),
                pool_addr,
                snapshot_block,
            )
            .await
            .map_err(|err| match err {
                RegistrationLifecycleError::Verify(v) => map_liquidity_verify_error(v),
                RegistrationLifecycleError::MissingProvider => {
                    crate::bot::engine::VerificationRpcError::new_err(err.to_string())
                }
                RegistrationLifecycleError::MissingStateView => {
                    pyo3::exceptions::PyValueError::new_err(err.to_string())
                }
            })
        })
    }

    /// V4 twin of [`run_v3_registration_lifecycle`] — runs the core-owned V4
    /// registration verify-lifecycle, keyed by (`pool_manager`, `pool_id`),
    /// using the stored `verify_state_view` contract address. A **tracked** V4
    /// pool with no `state_view` surfaced as `PyValueError` (D-C no-config
    /// fail-fast); Sparse pools never require `state_view`.
    #[expect(clippy::needless_pass_by_value)]
    pub(crate) fn run_v4_registration_lifecycle<'py>(
        &self,
        py: Python<'py>,
        pool_manager_address: String,
        pool_id_hex: String,
        snapshot_block: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let pool_manager: alloy::primitives::Address =
            pool_manager_address.parse().map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager: {e}"))
            })?;
        let pool_id = crate::bot::engine::hex_string_to_pool_id(&pool_id_hex).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_id: {e}"))
        })?;
        let state_view = *self.verify_state_view.lock();
        let core = {
            let engine = self.engine.lock();
            Arc::clone(engine.core())
        };
        let provider = self.verify_provider.lock().clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            use degenbot_bot::bot_core::registration_lifecycle::RegistrationLifecycleError;
            degenbot_bot::bot_core::run_v4_registration_lifecycle(
                &core,
                provider.as_ref(),
                pool_manager,
                pool_id,
                state_view,
                snapshot_block,
            )
            .await
            .map_err(|err| match err {
                RegistrationLifecycleError::Verify(v) => map_liquidity_verify_error(v),
                RegistrationLifecycleError::MissingProvider => {
                    crate::bot::engine::VerificationRpcError::new_err(err.to_string())
                }
                RegistrationLifecycleError::MissingStateView => {
                    pyo3::exceptions::PyValueError::new_err(err.to_string())
                }
            })
        })
    }
}

/// Map a `LiquidityVerifyError` (from `liquidity_verifier::verify_v3/v4_pools`)
/// to a typed Python exception, mirroring `engine::verify::map_verify_err`.
///
/// - `Mismatch` → `VerificationMismatchError` (fatal — on-chain tick data
///   disagrees with the engine).
/// - `Rpc` → `VerificationRpcError` (per-call RPC transport failure — the
///   caller may retry/backoff; NOT evidence of a mismatch).
///
/// Pre-AGVGNH the per-family `verify_v3/v4_liquidity_maps` methods mapped both
/// variants to a plain `PyRuntimeError`, which `build_paths`' broad
/// `except RuntimeError` arm silently swallowed as a skipped path — masking
/// genuine mismatches. Routing through this seam restores fail-fast: a
/// mismatch surfaces as `VerificationMismatchError`, the fatal arm in
/// `build_paths`.
pub(crate) fn map_liquidity_verify_error(
    err: degenbot_bot::bot_core::liquidity_verifier::LiquidityVerifyError,
) -> PyErr {
    use crate::bot::engine::{VerificationMismatchError, VerificationRpcError};
    use degenbot_bot::bot_core::liquidity_verifier::LiquidityVerifyError;
    match err {
        LiquidityVerifyError::Mismatch(m) => VerificationMismatchError::new_err(m.to_string()),
        LiquidityVerifyError::Rpc { message } => VerificationRpcError::new_err(message),
    }
}

#[expect(clippy::expect_used)]
#[cfg(test)]
mod tests {
    //! AGVGNH: pin the per-family verify exception mapping. The
    //! `verify_v3_liquidity_maps` / `verify_v4_liquidity_maps` methods must
    //! route `LiquidityVerifyError` through `map_liquidity_verify_error` so
    //! that a genuine on-chain mismatch surfaces as
    //! `VerificationMismatchError` (the fatal arm in `build_paths`) and a
    //! per-call RPC transport failure surfaces as `VerificationRpcError`
    //! (retryable), NOT a plain `PyRuntimeError` (which the broad
    //! `except RuntimeError` arm silently swallows as a skipped path).
    use super::map_liquidity_verify_error;
    use crate::bot::engine::{VerificationMismatchError, VerificationRpcError};
    use degenbot_bot::bot_core::liquidity_verifier::{LiquidityVerifyError, VerificationMismatch};

    #[test]
    fn mismatch_surfaces_as_verification_mismatch_error() {
        pyo3::Python::attach(|py| {
            let err =
                map_liquidity_verify_error(LiquidityVerifyError::Mismatch(VerificationMismatch {
                    message: "V3 pool 0x.. block=1: tick 5 liquidityGross mismatch".to_string(),
                }));
            assert!(
                err.is_instance_of::<VerificationMismatchError>(py),
                "LiquidityVerifyError::Mismatch must surface as VerificationMismatchError (fatal), not PyRuntimeError"
            );
            // Distinct from the RPC-error category.
            assert!(
                !err.is_instance_of::<VerificationRpcError>(py),
                "genuine mismatch is NOT an Rpc error (distinct types)"
            );
        });
    }

    #[test]
    fn rpc_failure_surfaces_as_verification_rpc_error() {
        pyo3::Python::attach(|py| {
            let err = map_liquidity_verify_error(LiquidityVerifyError::Rpc {
                message: "V3 pool 0x..: tickBitmap(0) RPC call failed: timeout".to_string(),
            });
            assert!(
                err.is_instance_of::<VerificationRpcError>(py),
                "LiquidityVerifyError::Rpc must surface as VerificationRpcError (retryable), not PyRuntimeError"
            );
            assert!(
                !err.is_instance_of::<VerificationMismatchError>(py),
                "RPC transport failure is NOT a mismatch (distinct types)"
            );
        });
    }

    // ── PumpState::stop() contract tests ─────────────────────────────
    //
    // A `stop()` is the symmetric teardown half of `subscribe()`/`resume()`:
    // it must set the shutdown flag (the cooperative signal the pump loop
    // checks each iteration), abort the spawned pump task (so the
    // `combined.next().await` — which would otherwise block up to 60s —
    // unblocks immediately), and be idempotent (a second call is a no-op
    // `Ok(())` because the handle is `take()`n on the first). Building a
    // real `PumpState` mirrors the standalone (no-`py_bot`) path of
    // `PyArbitrageEngine::new` — a fresh `Bot`/`BotState`/`ArbitrageEngine`/
    // `SolveCoordinator`/`ReorgCoordinator`. No WS connection is opened;
    // the running-handle test installs a never-completing dummy task so
    // `stop()`'s abort path is exercised without the real pump.

    use super::PumpState;
    use degenbot_bot::bot_core::reorg_coordinator::ReorgCoordinator;
    use degenbot_bot::bot_core::solve_coordinator::SolveCoordinator;
    use degenbot_bot::bot_core::{Bot, BotState};
    use degenbot_bot::solvers::arb_engine::engine_handle::EngineHandle;
    use degenbot_bot::solvers::arb_engine::ArbitrageEngine;
    use parking_lot::RwLock;
    use tokio::sync::mpsc;

    fn pump_state_for_test() -> std::sync::Arc<PumpState> {
        let core = std::sync::Arc::new(RwLock::new(BotState::new()));
        let bot = std::sync::Arc::new(Bot::with_core(std::sync::Arc::clone(&core)));
        let mut engine = ArbitrageEngine::with_core(core);
        let (result_tx, _result_rx) = mpsc::unbounded_channel();
        let (block_tx, _block_rx) = mpsc::unbounded_channel();
        engine.set_result_channel(result_tx);
        engine.set_block_channel(block_tx);
        let engine = std::sync::Arc::new(parking_lot::Mutex::new(engine));
        let coordinator = std::sync::Arc::new(SolveCoordinator::new(vec![EngineHandle::arc_dyn(
            std::sync::Arc::clone(&engine),
        )]));
        let reorg_coordinator =
            std::sync::Arc::new(ReorgCoordinator::new(std::sync::Arc::clone(&bot)));
        std::sync::Arc::new(PumpState::new(engine, coordinator, reorg_coordinator, bot))
    }

    #[test]
    fn stop_pre_resume_sets_shutdown_flag_and_clears_subscribe_state() {
        // Red-first: with no pump running (pre-`resume()`), `stop()` must
        // still flip the cooperative shutdown flag and clear any
        // half-built subscribe state, returning `Ok(())`. The pump handle
        // is `None` so the abort path is skipped — the flag is the only
        // observable effect.
        let pump = pump_state_for_test();
        assert!(
            pump.pump_handle.lock().is_none(),
            "pre-resume: no pump task spawned"
        );
        assert!(
            !pump.shutdown.load(std::sync::atomic::Ordering::Relaxed),
            "shutdown flag starts false"
        );

        pyo3::Python::attach(|_| {
            pump.stop().expect("stop() before resume must not error");
        });

        assert!(
            pump.shutdown.load(std::sync::atomic::Ordering::Relaxed),
            "stop() must set the shutdown flag"
        );
        assert!(
            pump.pump_handle.lock().is_none(),
            "stop() must leave pump_handle None"
        );
        assert!(
            pump.subscribe_state.lock().is_none(),
            "stop() must clear subscribe_state"
        );
    }

    #[test]
    fn stop_is_idempotent() {
        // Calling stop() twice must not panic or error — the handle is
        // `take()`n on the first call so the second is a bare flag store +
        // no-op. This is the contract the Python `__aexit__` /
        // KeyboardInterrupt handler relies on (both may call stop()).
        let pump = pump_state_for_test();
        pyo3::Python::attach(|_| {
            pump.stop().expect("first stop() must not error");
            pump.stop().expect("second stop() must be a no-op Ok");
        });
        assert!(
            pump.shutdown.load(std::sync::atomic::Ordering::Relaxed),
            "shutdown flag stayed set across both calls"
        );
    }

    #[test]
    fn stop_aborts_a_running_pump_handle() {
        // The pump loop blocks on `combined.next().await`, which can hang up
        // to `BACKFILL_TIMEOUT_SECS` (60s) idle before the loop revisits its
        // shutdown check. `stop()` must abort the spawned task so the await
        // unblocks immediately, not wait for the flag to be observed at the
        // next loop iteration. We install a never-completing dummy task (a
        // pending oneshot receiver) as the `pump_handle`, then assert stop()
        // resolves it and returns promptly — a regression guard for the
        // "Ctrl-C hangs for a minute" bug.
        let pump = pump_state_for_test();
        let runtime = degenbot_core::runtime::get_runtime();
        let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = runtime.spawn(async move {
            // Never receives — only an abort unblocks this. Mirrors a pump
            // blocked on a silent WS subscription.
            let _ = rx.await;
        });
        *pump.pump_handle.lock() = Some(handle);
        assert!(
            pump.pump_handle.lock().is_some(),
            "fixture installed a pump handle"
        );

        // Run stop() on a worker thread (mirrors production: Python calls it
        // from the main thread, NOT inside a `block_on` — nesting block_on on
        // the same runtime would panic). A join timeout surfaces a regression
        // (forgot abort → stop() blocks on the pump task) as a test failure
        // instead of an indefinite hang.
        let pump_clone = std::sync::Arc::clone(&pump);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<pyo3::PyResult<()>>();
        std::thread::spawn(move || {
            pyo3::Python::attach(|_| {
                let _ = done_tx.send(pump_clone.stop());
            });
        });
        let stop_result = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect(
                "stop() must return within 5s (abort must unblock the pump task) — it timed out",
            );
        stop_result.expect("stop() with a running handle must not error");

        assert!(
            pump.shutdown.load(std::sync::atomic::Ordering::Relaxed),
            "stop() set the shutdown flag"
        );
        assert!(
            pump.pump_handle.lock().is_none(),
            "stop() must consume the pump handle"
        );
    }
}
