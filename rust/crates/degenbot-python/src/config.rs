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
