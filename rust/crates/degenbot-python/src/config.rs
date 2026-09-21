//! Typed `BotConfig` accessors for the Python driver shell (4IOEVT).
//!
//! The 12-factor loader is the ONLY environment reader; it installs the
//! process-wide typed config through `degenbot_config::holder::install` at
//! `_ffi` import. These thin `#[pyfunction]` getters expose a typed field to
//! Python without introducing a second (parallel) declaration site.

use crate::prelude::*;

/// The typed `pathfinding.discovery_batch_size` value (env
/// `DEGENBOT_DISCOVERY_BATCH_SIZE`), positive-clamped to `>= 1` so a zero /
/// garbage value degrades to the legacy per-path delivery instead of a busy
/// loop.
#[pyfunction]
#[must_use]
pub fn discovery_batch_size() -> usize {
    ::degenbot_config::holder::config()
        .pathfinding
        .discovery_batch_size
        .max(1)
}

/// The shared core verification-retry policy defaults as a SELF-DESCRIBING
/// value — a positional 4-tuple would silently mis-assign on a Rust-side
/// field reorder:
/// in seconds for the float fields.
///
/// The Python `VerificationRetryPolicy` dataclass reads these instead of
/// carrying its own literal set, so the Rust `RetryPolicy`  is
/// the one declaration site for both the driver shell and the pure-Rust
/// example.
#[pyclass(frozen, get_all, module = "degenbot._ffi")]
pub struct RetryPolicyDefaults {
    pub max_attempts: u32,
    pub base_delay: f64,
    pub max_delay: f64,
    pub jitter: f64,
}

/// The resolved strategy readiness, exposed as a self-describing Python
/// view: the settlement and two per-ecosystem backrun arms with their settled
/// endpoint posture.
///
/// Built from the process-wide typed config through the SAME
/// `strategy_readiness` authority the operators' `degenbot strategy` verbs
/// and the backrun driver boot use, so the Python driver shell cannot disagree
/// with the console about what "settled" means.
#[pyclass(frozen, module = "degenbot._ffi")]
pub struct StrategyReadinessView {
    #[pyo3(get)]
    pub settlement_active: bool,
    #[pyo3(get)]
    pub settlement_endpoints: Vec<String>,
    #[pyo3(get)]
    pub mevblocker_backrun_active: bool,
    #[pyo3(get)]
    pub mevblocker_backrun_endpoints: Vec<String>,
    #[pyo3(get)]
    pub peer_backrun_active: bool,
    #[pyo3(get)]
    pub peer_backrun_endpoints: Vec<String>,
}

impl StrategyReadinessView {
    /// Build from the resolved arms (activity + resolved URLs).
    fn from_readiness(readiness: &::degenbot_config::StrategyReadiness) -> Self {
        fn arm(arm: &::degenbot_config::Arm) -> (bool, Vec<String>) {
            match arm {
                ::degenbot_config::Arm::Inactive => (false, Vec::new()),
                ::degenbot_config::Arm::Active(urls) => (true, urls.clone()),
            }
        }
        let (settlement_active, settlement_endpoints) = arm(&readiness.settlement);
        let (mevblocker_backrun_active, mevblocker_backrun_endpoints) =
            arm(&readiness.mevblocker_backrun);
        let (peer_backrun_active, peer_backrun_endpoints) = arm(&readiness.peer_backrun);
        Self {
            settlement_active,
            settlement_endpoints,
            mevblocker_backrun_active,
            mevblocker_backrun_endpoints,
            peer_backrun_active,
            peer_backrun_endpoints,
        }
    }
}

/// Resolve the strategy readiness of the installed typed config.
///
/// # Errors
///
/// `ValueError` carrying the typed refusal's remediation message (e.g. the
/// activation/endpoint remedies from the console verbs) — a live boot that
/// cannot settle STRATEGY endpoints refuses instead of degrading to the
/// public mempool.
#[pyfunction]
pub fn validate_strategy_readiness() -> PyResult<StrategyReadinessView> {
    ::degenbot_config::strategy_readiness(::degenbot_config::holder::config())
        .map(|readiness| StrategyReadinessView::from_readiness(&readiness))
        .map_err(|error| ::pyo3::exceptions::PyValueError::new_err(error.to_string()))
}

/// The resolved settlement broadcast endpoints (this process's settlement
/// arm). Raises `ValueError` when the settlement facet is not active — a
/// hosted runner is the settlement arm, so its broadcast posture is never
/// optional.
///
/// # Errors
///
/// `ValueError` when the facet is inactive.
#[pyfunction]
pub fn settlement_broadcast_endpoints() -> PyResult<Vec<String>> {
    let config = ::degenbot_config::holder::config();
    ::degenbot_config::strategy_readiness(config)
        .map_err(|error| ::pyo3::exceptions::PyValueError::new_err(error.to_string()))
        .and_then(|readiness| match &readiness.settlement {
            ::degenbot_config::Arm::Inactive => Err(::pyo3::exceptions::PyValueError::new_err(
                "strategy settlement is not active: this hosted runner IS the settlement arm; \
                 activate it first (degenbot strategy activate settlement --endpoints-default)",
            )),
            ::degenbot_config::Arm::Active(urls) => Ok(urls.clone()),
        })
}
/// Read the shared core verification-retry policy defaults.
#[pyfunction]
#[must_use]
pub fn verification_retry_policy_defaults() -> RetryPolicyDefaults {
    let policy = ::degenbot_core::retry::RetryPolicy::verification_default();
    RetryPolicyDefaults {
        max_attempts: policy.max_attempts,
        base_delay: policy.base_delay,
        max_delay: policy.max_delay,
        jitter: policy.jitter,
    }
}
