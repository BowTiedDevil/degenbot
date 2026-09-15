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
/// value (TD5/P2 — the former anonymous 4-tuple would silently mis-assign on
/// a Rust-side field reorder):
/// in seconds for the float fields.
///
/// The Python `VerificationRetryPolicy` dataclass reads these instead of
/// carrying its own literal set, so the Rust `RetryPolicy` (ergo 6LC4JB) is
/// the one declaration site for both the driver shell and the pure-Rust
/// example.
#[pyclass(frozen, get_all, module = "degenbot._ffi")]
pub struct RetryPolicyDefaults {
    pub max_attempts: u32,
    pub base_delay: f64,
    pub max_delay: f64,
    pub jitter: f64,
}

/// Read the shared core verification-retry policy defaults (6LC4JB).
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
