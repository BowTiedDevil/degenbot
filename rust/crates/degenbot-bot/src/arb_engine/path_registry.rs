//! The engine's path-identity registry (ADR-045, ergo task `C4UAFP`).
//!
//! `PathRegistry` owns *identity only*: the registered paths (`path_pools`),
//! the `pool_to_paths` reverse index, the signature dedup map, the monotonic
//! path-id allocator, the registered-path cap, and the dedup counter. It
//! deliberately holds no resolve or solve state and no dependency beyond the
//! solver value types (`MixedPath` / `MixedPoolRef` / `HopType`).
//!
//! Borrow contract (ADR-045): the hot solve cycle reads identity through a
//! **shared** `&PathRegistry` — cycles never mutate identity. Registration
//! (`commit` / `remove` / `note_dedup` / `set_cap`) is the only `&mut` caller:
//! reads take `&self`, the mutating registration surface takes `&mut self`.
//!
//! `commit` is all-or-nothing: it allocates the next path id, inserts the
//! immutable pool refs, extends the reverse index, and records the dedup
//! signature in one call, so `path_pools` and `pool_to_paths` cannot be
//! desynced by a partial registration. `remove` is the inverse: it drops the
//! path, prunes the reverse index, and clears the signature.
use ::degenbot_solvers::mixed::{HopType, MixedPath, MixedPoolRef};
use hashbrown::HashMap;
use std::sync::Arc;
/// Typed refusal from `ArbitrageEngine::register_path` (PRG-4 / IRUMXD — was a
/// bare `String`).
///
/// Moved here from `lifecycle` by ADR-045 (`C4UAFP`); re-exported at the old
/// `arb_engine::lifecycle` path so the `PyO3` mapper in `degenbot-python`
/// stays byte-identical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRegistrationError {
    /// Structural caller bug or stale view: fewer than two hops, a
    /// `pool_id` not registered in the associated `BotState`, or a
    /// structurally unroutable hop. Message text is unchanged from the
    /// legacy `String` form (the `PyO3` mapping surfaces it verbatim as a
    /// `ValueError`).
    Invalid(String),
    /// PRG-4: the registered-path cap is reached. A BENIGN stop signal, not
    /// an error condition — the crawl catches it and stops discovery (it
    /// replaces the Python `DiscoveryCrawlComplete` pre-count unwind).
    RegistryFull {
        /// The configured capacity.
        cap: usize,
        /// The registered-path count at refusal (== `cap`).
        registered: usize,
    },
}
impl std::fmt::Display for PathRegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(msg) => f.write_str(msg),
            Self::RegistryFull { cap, registered } => write!(
                f,
                "registered-path cap reached ({registered}/{cap}) — the crawl must stop discovery"
            ),
        }
    }
}
/// The identity payload a fresh registration commits to the registry.
///
/// Built by `register_path` after the hops have been validated and resolved;
/// `PathRegistry::commit` is the only consumer.
#[derive(Debug, Clone)]
pub(crate) struct PathRegistration {
    /// The canonical `(pool_id, zero_for_one)` hop sequence used for dedup
    /// and reverse-index reconstruction.
    pub(crate) signature: Vec<(u64, bool)>,
    /// The immutable per-hop pool refs stored in `path_pools`.
    pub(crate) pool_refs: Vec<MixedPoolRef>,
}
/// Path identity (ADR-045): registered paths + reverse index + signatures +
/// id allocator + cap + dedup counter. Deliberately SHALLOW — no resolve, no
/// solve, no deps beyond the solver value types.
pub(crate) struct PathRegistry {
    /// Registered path pool refs (immutable after registration).
    pub(crate) path_pools: HashMap<u64, Arc<MixedPath>>,
    /// Reverse index: (`hop_type`, `pool_key`) -> path ids using that pool.
    /// `Vec` instead of `HashSet` — sets are typically 1-4 entries, dedup at
    /// collection time.
    pub(crate) pool_to_paths: HashMap<(HopType, u64), Vec<u64>>,
    /// Dedup index: canonical `(pool_id, zero_for_one)` sequence -> path id.
    /// `register_path` is idempotent: re-registering the same hop sequence
    /// returns the existing `path_id` instead of allocating a new one
    /// (FPGOYX).
    path_signatures: HashMap<Vec<(u64, bool)>, u64>,
    /// Auto-incrementing path id (starts at 1).
    next_path_id: u64,
    /// Registered-path capacity; `None` = unlimited.
    path_cap: Option<usize>,
    /// Dedup hits counted engine-side (PRG-4).
    path_dedups: u64,
}
impl Default for PathRegistry {
    fn default() -> Self {
        Self::new()
    }
}
impl PathRegistry {
    /// A fresh, empty registry. Path ids start at 1.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            path_pools: HashMap::new(),
            pool_to_paths: HashMap::new(),
            path_signatures: HashMap::new(),
            next_path_id: 1,
            path_cap: None,
            path_dedups: 0,
        }
    }
    /// The existing path id for `signature`, or `None` when it is new.
    #[must_use]
    pub(crate) fn lookup(&self, signature: &[(u64, bool)]) -> Option<u64> {
        self.path_signatures.get(signature).copied()
    }
    /// Registered path refs for `path_id`.
    #[must_use]
    pub(crate) fn get(&self, path_id: u64) -> Option<&Arc<MixedPath>> {
        self.path_pools.get(&path_id)
    }
    /// The path ids that reference `key` (the hot fan-out read; shared).
    #[must_use]
    pub(crate) fn paths_for(&self, key: &(HopType, u64)) -> Option<&[u64]> {
        self.pool_to_paths.get(key).map(Vec::as_slice)
    }
    /// The full `path_id -> Arc<MixedPath>` map (the worker-snapshot clone
    /// source).
    #[must_use]
    pub(crate) fn path_pools(&self) -> &HashMap<u64, Arc<MixedPath>> {
        &self.path_pools
    }
    /// The number of registered paths.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.path_pools.len()
    }
    /// Whether the registry holds no paths.
    #[must_use]
    #[expect(dead_code)] // part of the ADR-045 registry surface; no caller yet
    pub(crate) fn is_empty(&self) -> bool {
        self.path_pools.is_empty()
    }
    /// Iterate the registered `(path_id, path)` pairs.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&u64, &Arc<MixedPath>)> {
        self.path_pools.iter()
    }
    /// Dedup hits counted engine-side.
    #[must_use]
    pub(crate) fn dedups(&self) -> u64 {
        self.path_dedups
    }
    /// Refuse a NEW registration once the cap is reached. A dedup hit never
    /// reaches this gate — an existing path is not growth.
    ///
    /// # Errors
    ///
    /// Returns `PathRegistrationError::RegistryFull` when the registered
    /// count is at or above the configured cap.
    pub(crate) fn ensure_capacity(&self) -> Result<(), PathRegistrationError> {
        if let Some(cap) = self.path_cap {
            let registered = self.path_pools.len();
            if registered >= cap {
                return Err(PathRegistrationError::RegistryFull { cap, registered });
            }
        }
        Ok(())
    }
    /// Commit a validated, freshly-resolved registration (all-or-nothing):
    /// allocate the id, insert the pool refs, extend the reverse index, and
    /// record the dedup signature. Returns the allocated path id.
    pub(crate) fn commit(&mut self, reg: PathRegistration) -> u64 {
        let path_id = self.next_path_id;
        self.next_path_id += 1;
        for pool_ref in &reg.pool_refs {
            self.pool_to_paths
                .entry((pool_ref.hop_type, pool_ref.pool_key))
                .or_default()
                .push(path_id);
        }
        self.path_pools.insert(
            path_id,
            Arc::new(MixedPath {
                pools: reg.pool_refs,
            }),
        );
        self.path_signatures.insert(reg.signature, path_id);
        path_id
    }
    /// Record a dedup hit (`register_path` returned an existing id).
    pub(crate) fn note_dedup(&mut self) {
        self.path_dedups += 1;
    }
    /// Deregister a path: drop it, prune the reverse index, and clear its
    /// dedup signature (so a later re-registration is a fresh register).
    pub(crate) fn remove(&mut self, path_id: u64) -> Option<Arc<MixedPath>> {
        let removed = self.path_pools.remove(&path_id);
        if let Some(path) = &removed {
            for pool_ref in &path.pools {
                if let Some(path_ids) = self
                    .pool_to_paths
                    .get_mut(&(pool_ref.hop_type, pool_ref.pool_key))
                {
                    path_ids.retain(|id| *id != path_id);
                }
            }
            let signature: Vec<(u64, bool)> = path
                .pools
                .iter()
                .map(|pool_ref| (pool_ref.pool_key, pool_ref.zero_for_one))
                .collect();
            self.path_signatures.remove(&signature);
        }
        removed
    }
    /// Set the registered-path cap (`None` = unlimited).
    pub(crate) fn set_cap(&mut self, cap: Option<usize>) {
        self.path_cap = cap;
    }
    /// Read the registered-path cap (`None` = unlimited) — the `EngineRetune`
    /// white-box observability surface.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn cap(&self) -> Option<usize> {
        self.path_cap
    }
}
