//! Driver-seam translation (`PyO3` side; ADR-050 D7 follow-up).
//!
//! The pump lifecycle ritual — `subscribe`, `resume` (which owns the
//! `S+1..W` auto-backfill), `stop`, the verify config, and the registration
//! lifecycles — lives ONCE in `degenbot-bot`'s public `EngineDriver`. The
//! `PyO3` layer holds `Arc<EngineDriver>` directly and crosses the driver
//! seam itself; the soak Drop forensics live on `EngineDriver` in the core.
//!
//! What remains here is translation, not state: the `GIL`-detach-
//! `block_on`/`future_into_py` wrappers plus the `DriverError` → typed Python
//! exception maps, as free functions both `PyBot` and `PyArbEngine` call with
//! their shared driver handle. The `subscribe`/`resume`/`stop` block_on
//! wrappers moved onto `bot_core::Bot` (the shells call them directly through
//! the `map_driver_err` seam); `start` and the registration lifecycles keep
//! their wrappers here.

use degenbot_bot::arb_engine::{DriverError, EngineDriver};
use degenbot_bot::bot_core::registration_lifecycle::RegistrationLifecycleError;
use degenbot_bot::bot_core::snapshot_verify::VerifyError;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::Bound;
use std::sync::Arc;

/// Run the pre-pump startup ritual: `subscribe(ws)` then verify-config
/// (`http`, optional `view`) — the one-call `EngineDriver::start` detached
/// from the GIL.
///
/// The snapshot seed `S` must already be on the shared `BotState`; the driver
/// reads it while subscribing, so `after_subscribe` can advance the phase to
/// `SnapshotLoaded`.
///
/// # Errors
/// `PyRuntimeError` if the phase is wrong, the pump is already
/// started/subscribed, the driver is stopped, or the WS subscribe fails.
pub(crate) fn start(
    py: Python<'_>,
    driver: &Arc<EngineDriver>,
    node_http: &str,
    node_ws: &str,
    verify_state_view: Option<&str>,
) -> PyResult<u64> {
    let driver = Arc::clone(driver);
    let node_http = node_http.to_string();
    let node_ws = node_ws.to_string();
    let verify_state_view = verify_state_view.map(str::to_string);
    // GIL-release across the WS handshake `block_on`: the handshake future
    // (WS subscribe + header polling + provider build) does NOT need the GIL.
    py.detach(move || {
        degenbot_core::runtime::get_runtime().block_on(driver.start(
            &node_http,
            &node_ws,
            verify_state_view.as_deref(),
        ))
    })
    .map_err(map_driver_err)
}

/// Awaitable session-end DETECTION FACT over [`EngineDriver::wait_session_end`].
///
/// Resolves the core [`SessionEndCause`](degenbot_bot::arb_engine::session_end::SessionEndCause)
/// name once the pump task finishes — cooperative timed exit
/// (`HOTPATH_SHUTDOWN_MS`), WS stream end, abort, or panic. A consumer that
/// awaits it AFTER the pump already ended still resolves (the completion is a
/// retained broadcast, not a one-shot signal consumed at creation).
///
/// # Errors
/// Only if the `PyO3` bridge itself fails to create the future.
pub(crate) fn session_end_future<'py>(
    py: Python<'py>,
    driver: &Arc<EngineDriver>,
) -> PyResult<Bound<'py, PyAny>> {
    let driver = Arc::clone(driver);
    crate::ambient_runtime::future_into_py(
        py,
        async move { Ok(driver.wait_session_end().await.name()) },
    )
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
    crate::ambient_runtime::future_into_py(py, async move {
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
/// with no verification provider surfaces as `VerificationRpcError`.
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
    crate::ambient_runtime::future_into_py(py, async move {
        driver
            .run_v4_registration_lifecycle(pool_manager, pool_id, snapshot_block)
            .await
            .map_err(map_driver_lifecycle_err)
    })
}

/// Blocking (GIL-detached) V3 verify-lifecycle — the seat-thread twin of
/// [`run_v3_registration_lifecycle`] (PRG-5).
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

/// Blocking (GIL-detached) V3 verify-lifecycle under the bounded retry dance.
///
/// The retry classification is core-owned (`VerifyError`); the caller (the
/// driver shell) resolves the policy and injects it here.
///
/// # Errors
///
/// `VerificationMismatchError` for a fatal snapshot mismatch,
/// `VerificationRpcError` for the last transient failure after exhausting the
/// policy, and `PyRuntimeError` for any other lifecycle failure.
pub(crate) fn run_v3_registration_lifecycle_with_retry_blocking(
    py: Python<'_>,
    driver: &Arc<EngineDriver>,
    address: &str,
    snapshot_block: Option<u64>,
    policy: &crate::config::RetryPolicy,
) -> PyResult<()> {
    let pool_addr: alloy::primitives::Address = address
        .parse()
        .map_err(|e| PyValueError::new_err(format!("Invalid V3 address: {e}")))?;
    let driver = Arc::clone(driver);
    let policy = policy.to_core();
    py.detach(move || {
        driver
            .run_v3_registration_lifecycle_with_retry_sync(pool_addr, snapshot_block, &policy)
            .map_err(map_verify_lifecycle_error)
    })
}

/// Blocking (GIL-detached) V4 verify-lifecycle under the bounded retry dance.
///
/// # Errors
///
/// As the V3 twin.
pub(crate) fn run_v4_registration_lifecycle_with_retry_blocking(
    py: Python<'_>,
    driver: &Arc<EngineDriver>,
    pool_manager_address: &str,
    pool_id_hex: &str,
    snapshot_block: Option<u64>,
    policy: &crate::config::RetryPolicy,
) -> PyResult<()> {
    let pool_manager: alloy::primitives::Address = pool_manager_address
        .parse()
        .map_err(|e| PyValueError::new_err(format!("Invalid pool_manager: {e}")))?;
    let pool_id = crate::bot::engine::hex_string_to_pool_id(pool_id_hex)
        .map_err(|e| PyValueError::new_err(format!("Invalid pool_id: {e}")))?;
    let driver = Arc::clone(driver);
    let policy = policy.to_core();
    py.detach(move || {
        driver
            .run_v4_registration_lifecycle_with_retry_sync(
                pool_manager,
                pool_id,
                snapshot_block,
                &policy,
            )
            .map_err(map_verify_lifecycle_error)
    })
}

/// Map a classified [`VerifyError`] from a retry-wrapped lifecycle to the typed
/// Python exception the verify surface has always raised.
pub(crate) fn map_verify_lifecycle_error(err: VerifyError) -> PyErr {
    use crate::bot::engine::{VerificationMismatchError, VerificationRpcError};
    match err {
        VerifyError::Snapshot(message) => VerificationMismatchError::new_err(message),
        VerifyError::Provider(message) | VerifyError::Rpc(message) => {
            VerificationRpcError::new_err(message)
        }
        VerifyError::Other(message) => PyRuntimeError::new_err(message),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// Map a core [`DriverError`] to a Python exception.
pub(crate) fn map_driver_err(err: DriverError) -> PyErr {
    match err {
        // The typed receiver refusal carries no payload; Display is the single
        // source of the remediation text, so the Python `RuntimeError` string
        // stays byte-identical to the pre-typing refusal.
        DriverError::NoResultReceiver => PyRuntimeError::new_err(err.to_string()),
        // Every other non-lifecycle variant (phase/session/subscribe/resume/
        // registration) surfaces as the legacy `RuntimeError` with the driver's
        // message; the registration lifecycles route their verify errors
        // through the typed `map_driver_lifecycle_err` instead.
        other => PyRuntimeError::new_err(other.to_string()),
    }
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
            RegistrationLifecycleError::MissingTickSpacing => PyRuntimeError::new_err(
                RegistrationLifecycleError::MissingTickSpacing.to_string(),
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
    //! Pin the per-family verify exception mapping. The
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
