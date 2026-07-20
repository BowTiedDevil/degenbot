//! `PyArbitrageEngine` — `path_info_for` core accessor (NXM2BF).
//!
//! [`PyArbitrageEngine::path_info_for_core`] exposes the core
//! [`ArbitrageEngine::path_info_for`] projection as a `pub(crate)` accessor so
//! `PyDispatchCandidate::__new__` (`crate::simulation::candidate`) can resolve
//! a path's `composers::PathInfo` from a registered `path_id` without a Python
//! `PathInfo` dataclass round-trip.
//!
//! No `#[pymethod]` mirror is exposed: `composers::PathInfo` is not a pyclass
//! (it's the Rust encode-side type), and the cockpit never calls this from
//! Python — only `PyDispatchCandidate::__new__` (Rust) reaches it via the
//! core accessor. A future Python-visible resolver (if the cockpit ever
//! needs to introspect a path's hops outside the candidate seam) would wrap
//! the `PathInfo` in a pyclass first.

use degenbot_executor::composers::PathInfo;

use degenbot_bot::solvers::arb_engine::path_info::PathInfoBuildError;

use super::PyArbitrageEngine;

impl PyArbitrageEngine {
    /// Core-path accessor (no GIL): resolve `path_id` to its
    /// `composers::PathInfo` via the engine's stored path + the shared
    /// `BotState` identities.
    ///
    /// `pub(crate)` so `PyDispatchCandidate::__new__` builds the held
    /// `PathInfo` without going through Python — the candidate the cockpit
    /// hands to `dispatch_profitable_py` is Rust-resolved end-to-end.
    #[must_use]
    pub(crate) fn path_info_for_core(
        &self,
        path_id: u64,
    ) -> Option<Result<PathInfo, PathInfoBuildError>> {
        self.engine.lock().path_info_for(path_id)
    }
}
