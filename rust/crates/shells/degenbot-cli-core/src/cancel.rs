//! The cooperative cancel carrier the updater arms thread into the cores.
//!
//! Pure-Rust twin of `degenbot._ffi.cancel.CancelHandle` (the `PyO3` class lives
//! in the binding layer, which this crate must not depend on). The argv facade
//! installs the SIGINT policy (ADR-051 D7: first Ctrl+C calls [`CancelHandle::cancel`];
//! a second restores the default disposition) and the cores poll the flag at
//! chunk boundaries — never mid-chunk, so chunk atomicity (commit OR rollback)
//! is preserved and committed chunks stay durable.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A shareable cooperative cancel flag.
#[derive(Debug, Clone, Default)]
pub struct CancelHandle {
    flag: Arc<AtomicBool>,
}

impl CancelHandle {
    /// A fresh, un-cancelled handle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the cancel flag (the SIGINT handler's job).
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// The underlying flag, for threading into `run_pool_update` /
    /// `run_aave_update`.
    #[must_use]
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }
}
