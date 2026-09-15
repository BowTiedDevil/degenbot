//! Bot-owned pump / lifecycle state (ADR-006 D4; ADR-050 D7 re-parent).
//!
//! **ADR-050 D7 re-parent:** the pump lifecycle ritual — `subscribe`,
//! `resume` (which owns the `S+1..W` auto-backfill), `stop`, the snapshot
//! seed, the verify config, and the registration lifecycles — now lives ONCE
//! in `degenbot-bot`'s public `EngineDriver`. This module's `PumpState` is
//! the thin PyBot-side adapter that holds `Arc<EngineDriver>` and delegates.
//!
//! Open-question resolution (ADR-050): `PumpState` was NOT fully dissolved into
//! `PyBot` + `EngineDriver` in this cutover. It survives as the shared
//! `Arc<PumpState>` vessel that `PyBot` (for `block_stream` / the pump
//! lifecycle methods) and `PyArbEngine` co-own; every ritual method is a
//! one-line delegation, so the session fields have collapsed into the driver
//! and the adapter carries no engine logic. Full dissolution is a mechanical
//! follow-up once every `PyBot` call site is touched.

use degenbot_bot::arb_engine::{DriverError, EngineDriver, EngineStages};
use degenbot_bot::bot_core::registration_lifecycle::RegistrationLifecycleError;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::Bound;
use std::sync::Arc;

/// Shared lifecycle state for the pump (ADR-006 D4).
///
/// Held by both `PyBot` (the D4 pump owner) and `PyArbEngine` (whose
/// snapshot/solve slices still read `phase` / the stage surface). One
/// allocation per chain — both wrappers carry `Arc<PumpState>` to the same
/// instance. ADR-050 D7: the session state itself now lives on
/// [`EngineDriver`]; this adapter only forwards.
pub(crate) struct PumpState {
    /// The public Rust driver seam (ADR-050) — the ONE owner of the pump
    /// session (bot, stages, reorg coordinator, shutdown, handle, subscribe
    /// state, verify provider, result/block channel ends).
    driver: Arc<EngineDriver>,
}

impl PumpState {
    #[must_use]
    pub(crate) fn new(driver: Arc<EngineDriver>) -> Self {
        Self { driver }
    }

    /// The engine's ONE stage surface (the observer/registration escape
    /// hatch) — `solve.rs` routes `PumpControl` through it.
    #[must_use]
    pub(crate) fn stages(&self) -> &Arc<EngineStages> {
        self.driver.stages()
    }

    /// Hand the block-clock receiver to Python (`PyBot::block_stream`) —
    /// once-only take; a second call finds `None`.
    pub(crate) fn take_block_receiver(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<degenbot_bot::arb_engine::BlockNotification>>
    {
        self.driver.take_block_receiver()
    }

    /// Subscribe to the WS `newHeads` + logs streams (delegates to the
    /// driver's `subscribe`).
    ///
    /// # Errors
    /// `PyRuntimeError` if the pump is already started/subscribed, the phase
    /// is wrong, or the WS subscribe fails.
    #[tracing::instrument(name = "degenbot.pump.subscribe", skip(self, py), fields(rpc_url = %rpc_url))]
    pub(crate) fn subscribe(&self, py: Python<'_>, rpc_url: &str) -> PyResult<u64> {
        let driver = Arc::clone(&self.driver);
        // GIL-release across the WS handshake `block_on`: the handshake future
        // (WS subscribe + header polling) does NOT need the GIL to complete,
        // so `py.detach` is safe (no re-entry deadlock). PyO3 0.29 renamed
        // `allow_threads` to `detach`.
        py.detach(|| degenbot_core::runtime::get_runtime().block_on(driver.subscribe(rpc_url)))
            .map_err(map_driver_err)
    }

    /// Resume the pump — begin normal WS processing (delegates to the
    /// driver, which owns the synchronous `S+1..W` auto-backfill before it
    /// spawns the live loop).
    ///
    /// # Errors
    /// `PyRuntimeError` if the phase is wrong, subscribe wasn't called, the
    /// driver is stopped, or it was already resumed.
    #[tracing::instrument(name = "degenbot.pump.resume", skip(self, py))]
    pub(crate) fn resume(&self, py: Python<'_>) -> PyResult<()> {
        let driver = Arc::clone(&self.driver);
        // GIL-release across the backfill `block_on`: the backfill
        // (`eth_getLogs` + `BotState` mutation) is pure Rust async and does
        // not need the GIL. PyO3 0.29 renamed `allow_threads` to `detach`.
        py.detach(|| degenbot_core::runtime::get_runtime().block_on(driver.resume()))
            .map_err(map_driver_err)
    }

    /// Stop the pump (delegates to the driver's any-phase, idempotent stop).
    ///
    /// # Errors
    /// Currently always `Ok`; the typed result keeps the surface symmetric.
    pub(crate) fn stop(&self) -> PyResult<()> {
        self.driver.stop().map_err(map_driver_err)
    }

    /// Awaitable pump-completion surface (the `PyO3` twin of
    /// [`EngineDriver::wait_pump_finished`]).
    ///
    /// Resolves when the spawned pump task stops — cooperative timed exit
    /// (`HOTPATH_SHUTDOWN_MS`), WS stream end, abort, or panic. The future
    /// also resolves for a consumer created AFTER the pump already ended.
    ///
    /// # Errors
    /// Only if the `PyO3` bridge itself fails to create the future.
    pub(crate) fn pump_finished_future<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let driver = Arc::clone(&self.driver);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            driver.wait_pump_finished().await;
            Ok(())
        })
    }

    // -- Verify config (ADR-006 D4 T4, re-parented onto the driver) ---------

    /// Set the HTTP RPC URL used for verification.
    pub(crate) fn set_verify_rpc_url(&self, rpc_url: &str) {
        self.driver.set_verify_rpc_url(rpc_url);
    }

    /// Set the `StateView` contract address for V4 verification.
    pub(crate) fn set_verify_state_view(&self, state_view_address: &str) {
        self.driver.set_verify_state_view(state_view_address);
    }

    /// Run a V3 pool's core-owned registration verify-lifecycle end-to-end.
    ///
    /// # Errors
    ///
    /// `VerificationMismatchError` (fatal tripwire) / `VerificationRpcError`
    /// (transient) from the underlying verify, or `VerificationRpcError` when
    /// no verify provider was configured.
    pub(crate) fn run_v3_registration_lifecycle<'py>(
        &self,
        py: Python<'py>,
        address: &str,
        snapshot_block: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let pool_addr: alloy::primitives::Address = address
            .parse()
            .map_err(|e| PyValueError::new_err(format!("Invalid V3 address: {e}")))?;
        let driver = Arc::clone(&self.driver);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            driver
                .run_v3_registration_lifecycle(pool_addr, snapshot_block)
                .await
                .map_err(map_driver_lifecycle_err)
        })
    }

    /// V4 twin of [`run_v3_registration_lifecycle`].
    ///
    /// # Errors
    ///
    /// As V3; a tracked V4 pool with no `state_view` surfaces as
    /// `PyValueError` (D-C no-config fail-fast).
    pub(crate) fn run_v4_registration_lifecycle<'py>(
        &self,
        py: Python<'py>,
        pool_manager_address: &str,
        pool_id_hex: &str,
        snapshot_block: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let pool_manager: alloy::primitives::Address = pool_manager_address
            .parse()
            .map_err(|e| PyValueError::new_err(format!("Invalid pool_manager: {e}")))?;
        let pool_id = crate::bot::engine::hex_string_to_pool_id(pool_id_hex)
            .map_err(|e| PyValueError::new_err(format!("Invalid pool_id: {e}")))?;
        let driver = Arc::clone(&self.driver);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            driver
                .run_v4_registration_lifecycle(pool_manager, pool_id, snapshot_block)
                .await
                .map_err(map_driver_lifecycle_err)
        })
    }

    /// Blocking (GIL-detached) V3 verify-lifecycle — the seat-thread twin of
    /// [`Self::run_v3_registration_lifecycle`] (PRG-5 / IRUMXD).
    ///
    /// # Errors
    ///
    /// As the async twin.
    pub(crate) fn run_v3_registration_lifecycle_blocking(
        &self,
        py: Python<'_>,
        address: &str,
        snapshot_block: Option<u64>,
    ) -> PyResult<()> {
        let pool_addr: alloy::primitives::Address = address
            .parse()
            .map_err(|e| PyValueError::new_err(format!("Invalid V3 address: {e}")))?;
        let driver = Arc::clone(&self.driver);
        py.detach(move || {
            driver
                .run_v3_registration_lifecycle_sync(pool_addr, snapshot_block)
                .map_err(map_driver_lifecycle_err)
        })
    }

    /// Blocking (GIL-detached) V4 verify-lifecycle — the seat-thread twin of
    /// [`Self::run_v4_registration_lifecycle`].
    ///
    /// # Errors
    ///
    /// As the async twin.
    pub(crate) fn run_v4_registration_lifecycle_blocking(
        &self,
        py: Python<'_>,
        pool_manager_address: &str,
        pool_id_hex: &str,
        snapshot_block: Option<u64>,
    ) -> PyResult<()> {
        let pool_manager: alloy::primitives::Address = pool_manager_address
            .parse()
            .map_err(|e| PyValueError::new_err(format!("Invalid pool_manager: {e}")))?;
        let pool_id = crate::bot::engine::hex_string_to_pool_id(pool_id_hex)
            .map_err(|e| PyValueError::new_err(format!("Invalid pool_id: {e}")))?;
        let driver = Arc::clone(&self.driver);
        py.detach(move || {
            driver
                .run_v4_registration_lifecycle_sync(pool_manager, pool_id, snapshot_block)
                .map_err(map_driver_lifecycle_err)
        })
    }
}

/// Map a core [`DriverError`] to a Python exception.
fn map_driver_err(err: DriverError) -> PyErr {
    // Every non-lifecycle variant (phase/session/subscribe/resume/registration)
    // surfaces as the legacy `RuntimeError` with the driver's message; the
    // registration lifecycles route their verify errors through the typed
    // `map_driver_lifecycle_err` instead.
    let message = err.to_string();
    drop(err);
    PyRuntimeError::new_err(message)
}

/// Map a lifecycle [`DriverError`] to the typed Python exception the verify
/// surface has always raised.
fn map_driver_lifecycle_err(err: DriverError) -> PyErr {
    match err {
        DriverError::Verify(e) => match e {
            RegistrationLifecycleError::Verify(v) => map_liquidity_verify_error(v),
            RegistrationLifecycleError::MissingProvider => {
                crate::bot::engine::VerificationRpcError::new_err(
                    "registration verify requires an RPC provider for tracked pools — configure the bot's single provider"
                        .to_string(),
                )
            }
            RegistrationLifecycleError::MissingStateView => PyValueError::new_err(
                "registration verify requires a StateView contract address for V4 pools".to_string(),
            ),
        },
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// Map a `LiquidityVerifyError` (from `liquidity_verifier::verify_v3/v4_pools`)
/// to a typed Python exception, mirroring `engine::verify::map_verify_err`.
///
/// - `Mismatch` → `VerificationMismatchError` (fatal — on-chain tick data
///   disagrees with the engine).
/// - `Rpc` → `VerificationRpcError` (per-call RPC transport failure — the
///   caller may retry/backoff; NOT evidence of a mismatch).
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

/// Soak-2026-08-22 forensics: name teardown paths that bypass [`Self::stop`].
/// The v4 TLS abort happened with zero logged shutdown initiator; this makes
/// wrapper destruction visible. If the pump task handle was still armed at
/// drop time, Python-side unwinding tore down the wrapper without calling
/// `stop()` - exactly the silent-exit shape we are hunting.
///
/// Leveling: only the bypassed-`stop()` shape is WARN — it is the anomaly this
/// drop hook exists to catch. A post-`stop()` drop (`driver.pump_handle_armed()
/// == false`) is the *healthy* path every session takes at exit; warning there
/// trains operators to dismiss the log line and buries the real signal.
impl Drop for PumpState {
    fn drop(&mut self) {
        if self.driver.pump_handle_armed() {
            degenbot_core::op_warn!(
                domain = pump,
                pump_task_still_armed = true,
                "PumpState dropped WITHOUT stop() - Python-side unwind bypassed graceful shutdown"
            );
        } else {
            degenbot_core::diag!(
                domain = pump,
                pump_task_still_armed = false,
                "PumpState dropped after stop()"
            );
        }
    }
}

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
}
