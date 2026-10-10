//! Driver-seam translation (`PyO3` side; ADR-050 D7 follow-up).
//!
//! The pump lifecycle ritual — `subscribe`, `resume` (which owns the
//! `S+1..W` auto-backfill), `stop`, the verify config, and the registration
//! lifecycles — lives ONCE in `degenbot-bot`'s public `EngineDriver`. The
//! `PyO3` layer holds `Arc<EngineDriver>` directly and crosses the driver
//! seam itself; the soak Drop forensics live on `EngineDriver` in the core.
//!
//! What remains here is translation, not state: the `GIL`-detach-
//! `block_on`/`future_into_py` wrappers, as free functions both `PyBot` and
//! `PyArbEngine` call with their shared driver handle (the typed `DriverError`
//! → Python-exception maps live in [`crate::bot::errmap`]). The
//! `subscribe`/`resume`/`stop` `block_on` wrappers moved onto `bot_core::Bot`
//! (the shells call them directly through the `map_driver_err` seam); `start`
//! keeps its wrapper here. The registration lifecycles are one value behind
//! one pymethod pair in [`crate::bot::registration_lifecycle`].

use degenbot_bot::arb_engine::EngineDriver;
use pyo3::prelude::*;
use pyo3::Bound;
use std::sync::Arc;

use crate::bot::errmap::map_driver_err;

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
