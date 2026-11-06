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
//! configuration, a composition and never a second ordering — and the verify
//! configuration wrappers ([`install_verify_provider`] /
//! [`install_verify_state_view`]) are the blocking bodies the two
//! `set_verify_*` pymethod pairs delegate to. The registration lifecycles are
//! one value behind one pymethod pair in
//! [`crate::bot::registration_lifecycle`].

use alloy::primitives::Address;
use degenbot_bot::arb_engine::EngineDriver;
use degenbot_bot::bot_core::build_verify_provider;
use pyo3::prelude::*;
use pyo3::Bound;
use std::sync::Arc;

use crate::bot::errmap::map_connect_err;

/// Parse a Python-supplied V4 `StateView` address at the seam boundary.
///
/// A malformed address raises `ValueError` HERE — before anything touches the
/// verify home's configuration — so an invalid `StateView` is never installed
/// as the zero address, and a refused parse leaves the configuration exactly
/// as it was.
///
/// # Errors
/// `ValueError` when `state_view_address` is not a hex address.
pub(crate) fn parse_state_view(state_view_address: &str) -> PyResult<Address> {
    state_view_address.parse().map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "set_verify_state_view: malformed StateView address {state_view_address:?}: {e}"
        ))
    })
}

/// Merge the `StateView` address into the verify home's construction values —
/// through the home's own `config()`/`configure()` door (ADR-093 D4), so the
/// bounded retry and any already-installed provider survive the merge.
pub(crate) fn install_verify_state_view(driver: &Arc<EngineDriver>, state_view: Address) {
    let verify = Arc::clone(driver.verify());
    let mut config = verify.config();
    config.state_view = Some(state_view);
    verify.configure(config);
}

/// Build the verify home's ONE provider from `rpc_url` and merge it into the
/// construction values. Awaited — a caller already on the runtime (the `start`
/// ritual) never nests a `block_on` — with [`install_verify_provider`] as the
/// blocking pymethod body over it.
///
/// # Errors
/// `RuntimeError` when the provider cannot be constructed; the verify home's
/// configuration is then left unchanged.
pub(crate) async fn configure_verify_provider(
    driver: &Arc<EngineDriver>,
    rpc_url: &str,
) -> PyResult<()> {
    let provider = build_verify_provider(rpc_url).await.map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!(
            "failed to create verification provider from {rpc_url}: {e}"
        ))
    })?;
    let verify = Arc::clone(driver.verify());
    let mut config = verify.config();
    config.provider = Some(provider);
    verify.configure(config);
    Ok(())
}

/// The blocking body behind the `set_verify_rpc_url` pymethods: GIL detached,
/// provider construction blocked at this adapter boundary (never inside the
/// core runtime).
///
/// # Errors
/// See [`configure_verify_provider`].
pub(crate) fn install_verify_provider(
    py: Python<'_>,
    driver: &Arc<EngineDriver>,
    rpc_url: &str,
) -> PyResult<()> {
    py.detach(|| {
        degenbot_core::runtime::get_runtime().block_on(configure_verify_provider(driver, rpc_url))
    })
}

/// Run the pre-pump startup ritual: `connect(ws)` then verify-config
/// (`http`, optional `view`), detached from the GIL.
///
/// The snapshot seed `S` must already be on the shared `BotState`: `run`
/// reads it to close the `S+1..W` gap, and no pump pose depends on its
/// presence (`connect` lands the table's `Connecting -> Subscribed`).
///
/// # Errors
/// `ValueError` on a malformed `StateView` — refused before the ritual runs,
/// leaving the verify home's configuration unchanged. `PyRuntimeError` if the
/// pose refuses the move (another lifecycle move in flight, a terminal
/// session), the WS handshake fails, or the verify provider cannot be built.
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
    // Argument validation FIRST: a malformed `StateView` refuses before the
    // ritual runs, and can never land as the zero address.
    let state_view = verify_state_view.map(parse_state_view).transpose()?;
    // GIL-release across the WS handshake `block_on`: the handshake future
    // (WS subscribe + header polling + provider build) does NOT need the GIL.
    //
    // The ritual is an ADAPTER over two core doors: `connect` (the lifecycle
    // move, which deliberately carries no configuration — ADR-093 D1) and the
    // verify home's own `config()`/`configure()` door.
    py.detach(move || {
        degenbot_core::runtime::get_runtime().block_on(async move {
            let connected = driver.connect(&node_ws).await.map_err(map_connect_err)?;
            configure_verify_provider(&driver, &node_http).await?;
            if let Some(state_view) = state_view {
                install_verify_state_view(&driver, state_view);
            }
            Ok(connected.link_block)
        })
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
    clippy::expect_used,
    reason = "tests assert on the seam's known-valid names and fail-loud paths"
)]
mod tests {
    use super::*;

    use crate::bot::PyBot;
    use degenbot_bot::bot_core::Bot;

    /// A driver over a fresh bot — the verify home the setters configure.
    fn driver() -> Arc<EngineDriver> {
        Arc::new(EngineDriver::new(
            Arc::new(Bot::new(1)),
            degenbot_config::holder::config_arc(),
        ))
    }

    /// The verify setters fail LOUD on a bot with no engine attached: the
    /// missing-engine case raises at the adapter instead of swallowing the
    /// call as a no-op.
    #[test]
    fn the_verify_setters_raise_without_an_attached_engine() {
        Python::attach(|py| {
            let bot = PyBot::new(1);
            let err = bot
                .set_verify_rpc_url(py, "http://127.0.0.1:8545")
                .unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyRuntimeError>(py));
            let err = bot
                .set_verify_state_view("0x0000000000000000000000000000000000000001")
                .unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyRuntimeError>(py));
        });
    }

    /// A dead verify endpoint refuses AT the setter: the provider build's
    /// failure raises immediately and installs nothing on the verify home.
    #[test]
    fn a_dead_verify_endpoint_raises_immediately_and_installs_nothing() {
        Python::attach(|py| {
            let bot = PyBot::new(1);
            let driver = driver();
            bot.attach_pump_state(Arc::clone(&driver));
            let err = bot.set_verify_rpc_url(py, "ws://127.0.0.1:1").unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyRuntimeError>(py));
            assert!(
                driver.verify().config().provider.is_none(),
                "a refused provider build leaves the verify home unconfigured"
            );
        });
    }

    /// A malformed `StateView` raises `ValueError` and leaves the verify
    /// home's configuration unchanged — the invalid address never becomes
    /// the zero address.
    #[test]
    fn a_malformed_state_view_raises_value_error_and_changes_nothing() {
        Python::attach(|py| {
            let bot = PyBot::new(1);
            let driver = driver();
            bot.attach_pump_state(Arc::clone(&driver));
            bot.set_verify_state_view("0x0000000000000000000000000000000000000001")
                .expect("a valid view installs");
            let installed = driver.verify().config().state_view;

            let err = bot.set_verify_state_view("not-an-address").unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyValueError>(py));
            assert_eq!(
                driver.verify().config().state_view,
                installed,
                "a refused parse leaves the configuration unchanged"
            );
            assert_ne!(installed, Some(alloy::primitives::Address::ZERO));
        });
    }

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
