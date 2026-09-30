//! The driver spawn seam: the exit vocabulary, loop-future, and spawn-factory
//! types a host hands a strategy driver.
//!
//! These are pure mechanics (a future, an exit enum, a once-only factory and
//! the lane namespace it is handed), so they live in the substrate both the
//! host and the strategy crates compose as peers — a strategy boot can mint
//! its spawn factory without depending on any host crate.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use crate::nonce::StrategyId;

/// A lane's run-artifact namespace under the host state root.
///
/// A strategy that owns process-lifetime artifacts writes them under its own
/// name so two drivers on one host cannot collide on a shared path. The value
/// carries only the root; each consumer appends its own file names, so the
/// host stays free of the submission crate's file vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneNamespace {
    root: PathBuf,
}

impl LaneNamespace {
    /// The namespace `<state_root>/<id>`.
    #[must_use]
    pub fn under(state_root: impl Into<PathBuf>, id: &StrategyId) -> Self {
        Self {
            root: state_root.into().join(id.as_str()),
        }
    }

    /// The lane's root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The lane's session directory.
    #[must_use]
    pub fn session_dir(&self) -> PathBuf {
        self.root.join("session")
    }

    /// The lane's quarantine directory.
    #[must_use]
    pub fn quarantine_dir(&self) -> PathBuf {
        self.root.join("quarantine")
    }
}

/// Why a driver's loop returned, in the host's vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverExit {
    /// The loop stopped on request; no tombstone.
    Stopped,
    /// The loop halted itself; the cause becomes the tombstone detail.
    Halted(String),
}

/// A driver's loop future, minted by its spawn factory.
///
/// Not required to be `Send`: a lane whose replay stack is single-threaded (the
/// backrun lane's `Rc`-backed buffers) is polled inline on one blocking thread
/// using the ambient multi-thread runtime by the host's `start_driving`,
/// while the factory itself must be `Send` so it can travel to that thread.
/// The ambient multi-thread runtime is what `revm`'s `WrapDatabaseAsync::new`
/// requires; a dedicated current-thread runtime captures no handle and forbids
/// block-in-place, failing every `BlockSimHandle::build`.
pub type DriverFuture = Pin<Box<dyn Future<Output = DriverExit> + 'static>>;

/// The once-only factory that boots a driver's loop.
///
/// The host hands the factory the lane namespace for the driver it is starting
/// (`None` when no state root is installed), so a lane that writes
/// run-artifacts under its own name learns its scope at the driving edge and a
/// lane that keeps the process-global root is told so explicitly.
pub type DriverSpawnFactory = Box<dyn FnOnce(Option<LaneNamespace>) -> DriverFuture + Send>;
