//! Driver-seam translation (`PyO3` side; ADR-050 D7 follow-up, C5).
//!
//! The pump lifecycle ritual — `subscribe`, `resume` (which owns the
//! `S+1..W` auto-backfill), `stop`, the verify config, and the registration
//! lifecycles — lives ONCE in `degenbot-bot`'s public `EngineDriver`. The
//! `PyO3` layer holds `Arc<EngineDriver>` directly and crosses the driver seam
//! itself; the former `PumpState` delegation vessel (every method a one-line
//! delegate — an extra seam with no behavior per unit of interface) was
//! dissolved (C5) and its soak Drop forensics moved onto `EngineDriver` in
//! the core.
//!
//! What remains here is translation, not state: the `GIL`-detach-
//! `block_on`/`future_into_py` wrappers plus the `DriverError` → typed Python
//! exception maps, as free functions both `PyBot` and `PyArbEngine` call with
//! their shared driver handle.

use degenbot_bot::arb_engine::{DriverError, EngineDriver};
use degenbot_bot::bot_core::registration_lifecycle::RegistrationLifecycleError;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::Bound;
use std::sync::Arc;

/// Subscribe to the WS `newHeads` + logs streams (drives the driver's
/// `subscribe` detached from the GIL).
///
/// # Errors
/// `PyRuntimeError` if the pump is already started/subscribed, the phase
/// is wrong, or the WS subscribe fails.
pub(crate) fn subscribe(
    py: Python<'_>,
    driver: &Arc<EngineDriver>,
    rpc_url: &str,
) -> PyResult<u64> {
    let driver = Arc::clone(driver);
    // GIL-release across the WS handshake `block_on`: the handshake future
    // (WS subscribe + header polling) does NOT need the GIL to complete.
    py.detach(|| degenbot_core::runtime::get_runtime().block_on(driver.subscribe(rpc_url)))
        .map_err(map_driver_err)
}

/// Resume the pump — begin normal WS processing (drives the driver, which
/// owns the synchronous `S+1..W` auto-backfill before spawning the live
/// loop).
///
/// # Errors
/// `PyRuntimeError` if the phase is wrong, subscribe wasn't called, the
/// driver is stopped, or it was already resumed.
pub(crate) fn resume(py: Python<'_>, driver: &Arc<EngineDriver>) -> PyResult<()> {
    let driver = Arc::clone(driver);
    // GIL-release across the backfill `block_on`: the backfill
    // (`eth_getLogs` + `BotState` mutation) is pure Rust async and does
    // not need the GIL.
    py.detach(|| degenbot_core::runtime::get_runtime().block_on(driver.resume()))
        .map_err(map_driver_err)
}

/// Stop the pump (the driver's any-phase, idempotent stop).
///
/// # Errors
/// Currently always `Ok`; the typed result keeps the surface symmetric.
pub(crate) fn stop(driver: &Arc<EngineDriver>) -> PyResult<()> {
    driver.stop().map_err(map_driver_err)
}

/// Awaitable pump-completion surface (the `PyO3` bridge
/// over [`EngineDriver::wait_pump_finished`]).
///
/// # Errors
/// Only if the `PyO3` bridge itself fails to create the future.
pub(crate) fn pump_finished_future<'py>(
    py: Python<'py>,
    driver: &Arc<EngineDriver>,
) -> PyResult<Bound<'py, PyAny>> {
    let driver = Arc::clone(driver);
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        driver.wait_pump_finished().await;
        Ok(())
    })
}

/// Run a V3 pool's core-owned registration verify-lifecycle end-to-end.
///
/// # Errors
///
/// `ValueError` on a bad address; `VerificationMismatchError` (fatal
/// tripwire) / `VerificationRpcError` (transient) from the underlying
/// verify, or `VerificationRpcError` when no verify provider was configured.
pub(crate) fn run_v3_registration_lifecycle<'py>(
    py: Python<'py>,
    driver: &Arc<EngineDriver>,
    address: &str,
    snapshot_block: Option<u64>,
) -> PyResult<Bound<'py, PyAny>> {
    let pool_addr: alloy::primitives::Address = address
        .parse()
        .map_err(|e| PyValueError::new_err(format!("Invalid V3 address: {e}")))?;
    let driver = Arc::clone(driver);
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
/// `ValueError` on a bad address/pool id; otherwise as V3. A tracked V4 pool
/// with no `state_view` surfaces as `PyValueError` (D-C no-config fail-fast).
pub(crate) fn run_v4_registration_lifecycle<'py>(
    py: Python<'py>,
    driver: &Arc<EngineDriver>,
    pool_manager_address: &str,
    pool_id_hex: &str,
    snapshot_block: Option<u64>,
) -> PyResult<Bound<'py, PyAny>> {
    let pool_manager: alloy::primitives::Address = pool_manager_address
        .parse()
        .map_err(|e| PyValueError::new_err(format!("Invalid pool_manager: {e}")))?;
    let pool_id = crate::bot::engine::hex_string_to_pool_id(pool_id_hex)
        .map_err(|e| PyValueError::new_err(format!("Invalid pool_id: {e}")))?;
    let driver = Arc::clone(driver);
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        driver
            .run_v4_registration_lifecycle(pool_manager, pool_id, snapshot_block)
            .await
            .map_err(map_driver_lifecycle_err)
    })
}

/// Blocking (GIL-detached) V3 verify-lifecycle — the seat-thread twin of
/// [`run_v3_registration_lifecycle`] (PRG-5 / IRUMXD).
///
/// # Errors
///
/// As the async twin.
pub(crate) fn run_v3_registration_lifecycle_blocking(
    py: Python<'_>,
    driver: &Arc<EngineDriver>,
    address: &str,
    snapshot_block: Option<u64>,
) -> PyResult<()> {
    let pool_addr: alloy::primitives::Address = address
        .parse()
        .map_err(|e| PyValueError::new_err(format!("Invalid V3 address: {e}")))?;
    let driver = Arc::clone(driver);
    py.detach(move || {
        driver
            .run_v3_registration_lifecycle_sync(pool_addr, snapshot_block)
            .map_err(map_driver_lifecycle_err)
    })
}

/// Blocking (GIL-detached) V4 verify-lifecycle — the seat-thread twin of
/// [`run_v4_registration_lifecycle`].
///
/// # Errors
///
/// As the async twin.
pub(crate) fn run_v4_registration_lifecycle_blocking(
    py: Python<'_>,
    driver: &Arc<EngineDriver>,
    pool_manager_address: &str,
    pool_id_hex: &str,
    snapshot_block: Option<u64>,
) -> PyResult<()> {
    let pool_manager: alloy::primitives::Address = pool_manager_address
        .parse()
        .map_err(|e| PyValueError::new_err(format!("Invalid pool_manager: {e}")))?;
    let pool_id = crate::bot::engine::hex_string_to_pool_id(pool_id_hex)
        .map_err(|e| PyValueError::new_err(format!("Invalid pool_id: {e}")))?;
    let driver = Arc::clone(driver);
    py.detach(move || {
        driver
            .run_v4_registration_lifecycle_sync(pool_manager, pool_id, snapshot_block)
            .map_err(map_driver_lifecycle_err)
    })
}

/// Map a core [`DriverError`] to a Python exception.
pub(crate) fn map_driver_err(err: DriverError) -> PyErr {
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
pub(crate) fn map_driver_lifecycle_err(err: DriverError) -> PyErr {
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
