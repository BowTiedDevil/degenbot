//! `degenbot_rs.CancelHandle` — the cooperative cancel flag for the
//! long-running updater chunk loops (`run_pool_update`, `run_aave_update`).
//!
//! Extracted from `pool/mod.rs` (its first consumer) so the Aave updater seam
//! (`aave_updater.rs`) reuses it without depending on the `pool` feature. The
//! cancel flag is a generic primitive (an `Arc<AtomicBool>`-backed handle),
//! NOT pool-specific — it lived in `pool/mod.rs` only because the pool seam
//! was the first consumer.
//!
//! Registered on the top-level `degenbot_rs` module by [`register_cancel`]
//! (gated on `any(feature = "pool", feature = "aave-updater")`) so the
//! `from degenbot.degenbot_rs import CancelHandle` import path stays the same
//! regardless of which updater features are enabled (backwards-compatible).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyModule;

/// `degenbot_rs.CancelHandle` — the cooperative cancel flag for the
/// long-running updater loops (`run_pool_update`, `run_aave_update`).
///
/// Construct up front; pass to `run_pool_update` / `run_aave_update`; a
/// `signal.SIGINT` handler (installed by the CLI driver) calls `.cancel()` to
/// stop the run at the next chunk boundary. The loop polls the flag between
/// chunks (NOT mid-chunk) so a cancel never breaks the chunk's atomicity — the
/// in-flight chunk completes (commit or rollback) before the run returns, the
/// committed chunks stay durable.
///
/// This is preferred over a module-level `set_cancel()` accessor (a
/// `OnceLock<Arc<AtomicBool>>`): the handle is per-run (no global mutable
/// state, no race between concurrent runs, no stale global stuck "set" after
/// a run), + Python owns the lifetime (drop after the run). Mirrors the
/// `Arc<AtomicBool>` the core `run_pool_update` / `run_aave_update` take.
#[pyclass(name = "CancelHandle", module = "degenbot._ffi.cancel")]
pub struct CancelHandle {
    /// The shared flag the chunk loop polls between chunks. `Arc`-cloned into
    /// the core call (both `run_pool_update` + `run_aave_update` take
    /// `Arc<AtomicBool>`).
    pub(crate) flag: Arc<AtomicBool>,
}

#[pymethods]
impl CancelHandle {
    /// Construct a fresh cancel handle (flag initially `false`).
    #[new]
    fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation. Honored at the next chunk boundary (after the
    /// current chunk completes atomically). Idempotent.
    fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    /// Whether cancellation was requested. Useful for tests + for a CLI
    /// driver that wants to observe whether its own SIGINT handler fired.
    fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Reset the flag to `false` (reuse the handle for a fresh run).
    /// Not normally needed (construct a fresh handle per run), but offered
    /// for the test harness + the "run, observe, re-run" CLI shell.
    fn reset(&self) {
        self.flag.store(false, Ordering::Release);
    }
}

impl Default for CancelHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Register `CancelHandle` on the top-level `degenbot_rs` module. Called once
/// from `c_api::register` under `any(feature = "pool", feature =
/// "aave-updater")` — whichever updater seam is enabled needs the class
/// registered. No-op-safe when both are enabled (a single `add_class` call).
///
/// # Errors
///
/// Returns a [`PyErr`] if the `add_class` call fails (a name collision with
/// an already-registered `CancelHandle`); propagated unchanged to the
/// `#[pymodule]` caller.
pub fn register_cancel(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.cancel")?;
    submod.add_class::<CancelHandle>()?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.cancel", &submod)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `CancelHandle` round-trips: fresh → not cancelled; after `cancel()` →
    /// cancelled; after `reset()` → not. (No Python needed — pure Rust state.)
    #[test]
    fn cancel_handle_round_trip() {
        let h = CancelHandle::new();
        assert!(!h.is_cancelled());
        h.cancel();
        assert!(h.is_cancelled());
        h.reset();
        assert!(!h.is_cancelled());
    }
}
