//! Driver-seam translation (`PyO3` side; ADR-050 D7 follow-up, ADR-093).
//!
//! The session lifecycle — `connect` (the handshake plus the first complete
//! block), `run` (which owns the `S+1..W` auto-backfill and arms the live
//! loop), and the total `close` — lives ONCE in `degenbot-bot`'s public
//! `EngineDriver`, and the verify configuration lives on the verify home. The
//! `PyO3` layer holds `Arc<EngineDriver>` directly and crosses the driver
//! seam itself; the soak Drop forensics live on `EngineDriver` in the core.
//!
//! What remains here is translation, not state: the `GIL`-detach-
//! `block_on`/`future_into_py` wrappers, as free functions both `PyBot` and
//! `PyArbEngine` call with their shared driver handle (the typed `DriverError`
//! → Python-exception maps live in [`crate::bot::errmap`]). The `start`
//! wrapper is this shell's one-call ritual — `connect` plus the verify home's
//! configuration, a composition and never a second ordering. The registration
//! lifecycles are one value behind one pymethod pair in
//! [`crate::bot::registration_lifecycle`].

use degenbot_bot::arb_engine::EngineDriver;
use pyo3::prelude::*;
use pyo3::Bound;
use std::sync::Arc;

use crate::bot::errmap::map_connect_err;

/// Run the pre-pump startup ritual: `connect(ws)` then verify-config
/// (`http`, optional `view`), detached from the GIL.
///
/// The snapshot seed `S` must already be on the shared `BotState`: `run`
/// reads it to close the `S+1..W` gap, and no pump pose depends on its
/// presence (`connect` lands the table's `Connecting -> Subscribed`).
///
/// # Errors
/// `PyRuntimeError` if the pose refuses the move (another lifecycle move in
/// flight, a terminal session) or the WS handshake fails.
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
    //
    // The ritual is an ADAPTER over two core doors: `connect` (the lifecycle
    // move, which deliberately carries no configuration — ADR-093 D1) and the
    // verify home's own configuration door.
    py.detach(move || {
        let connected = degenbot_core::runtime::get_runtime()
            .block_on(driver.connect(&node_ws))
            .map_err(map_connect_err)?;
        degenbot_bot::bot_core::Bot::set_verify_rpc_url(Some(&driver), &node_http);
        if let Some(view) = verify_state_view.as_deref() {
            degenbot_bot::bot_core::Bot::set_verify_state_view(Some(&driver), view);
        }
        Ok(connected.link_block)
    })
}

/// Awaitable session-end DETECTION FACT over [`EngineDriver::wait_session_end`].
///
/// Resolves the core [`SessionEndCause`](degenbot_bot::arb_engine::SessionEndCause)
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

/// The core Session-end ranking over the stable fact spellings — the Python
/// session watch's ONE verdict source (never a local task-completion table).
///
/// `cause` is the detection cause name (`PumpFinished` /
/// `StallWatchdogTripped`), `consumer` the translated consumer outcome
/// (`QuietEnd` / `PumpStopped` / `Failed`), `registration` the registration
/// outcome (`Completed` / `Failed`); `None` records an absent fact. Returns
/// the ranked `(verdict, raise_slot)` names — the core's stable spellings
/// (`PumpEnded` / `RegistrationFailed` / `WatchdogTripped`, and `Nothing` /
/// `Registration` / `Consumer` / `SessionEnd`).
///
/// # Errors
///
/// `ValueError` when a fact name is outside the core's closed vocabulary.
#[pyfunction]
pub(crate) fn session_end_rank(
    cause: Option<&str>,
    consumer: Option<&str>,
    registration: Option<&str>,
) -> PyResult<(&'static str, &'static str)> {
    use degenbot_bot::arb_engine::{
        rank, ConsumerOutcome, RegistrationOutcome, SessionEndCause, SessionEndObservation,
    };

    let cause = cause
        .map(|name| {
            SessionEndCause::from_name(name).ok_or_else(|| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown session-end cause {name:?}"
                ))
            })
        })
        .transpose()?;
    let consumer = consumer
        .map(|name| {
            ConsumerOutcome::from_name(name).ok_or_else(|| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown consumer outcome {name:?}"
                ))
            })
        })
        .transpose()?;
    let registration = registration
        .map(|name| {
            RegistrationOutcome::from_name(name).ok_or_else(|| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown registration outcome {name:?}"
                ))
            })
        })
        .transpose()?;
    let ranking = rank::<()>(SessionEndObservation {
        cause,
        consumer,
        registration,
        output: None,
    });
    Ok((ranking.verdict.name(), ranking.slot.name()))
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "tests assert on the seam's known-valid names"
)]
mod tests {
    use super::*;

    /// The seam folds through the core's own vocabulary: the names in are the
    /// names the core parses, the names out are the names it emits.
    #[test]
    fn the_rank_seam_round_trips_the_core_vocabularies() {
        assert_eq!(
            session_end_rank(Some("PumpFinished"), Some("QuietEnd"), None).unwrap(),
            ("PumpEnded", "Nothing")
        );
        assert_eq!(
            session_end_rank(None, Some("PumpStopped"), Some("Completed")).unwrap(),
            ("WatchdogTripped", "SessionEnd")
        );
        assert_eq!(
            session_end_rank(Some("StallWatchdogTripped"), Some("Failed"), Some("Failed")).unwrap(),
            ("RegistrationFailed", "Registration")
        );
        assert!(session_end_rank(Some("nope"), None, None).is_err());
        assert!(session_end_rank(None, Some("nope"), None).is_err());
        assert!(session_end_rank(None, None, Some("nope")).is_err());
    }
}
