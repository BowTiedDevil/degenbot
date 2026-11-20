//! The engine's path-identity registry .
//!
//! `PathRegistry` owns *identity only*: one [`PathEntry`] per registered path
//! (the immutable hop refs, the canonical signature, and the session's object
//! for the route), the `pool_to_paths` reverse index, the signature dedup map,
//! the monotonic path-id allocator, the registered-path cap, and the dedup
//! counter. It deliberately holds no resolve or solve state and no dependency
//! beyond the solver value types (`MixedPath` / `MixedPoolRef` / `HopType`).
//!
//! Borrow contract (ADR-045): the hot solve cycle reads identity through a
//! **shared** `&PathRegistry` — cycles never mutate identity. Registration
//! (`commit` / `remove` / `note_dedup` / `set_cap`) is the only `&mut` caller:
//! reads take `&self`, the mutating registration surface takes `&mut self`.
//!
//! `commit` is all-or-nothing: it allocates the next path id, inserts the
//! entry (immutable pool refs + dedup signature), extends the reverse index,
//! and keys the dedup index in one call, so the path map and `pool_to_paths`
//! cannot be desynced by a partial registration. `remove` is the inverse: it
//! drops the entry, prunes every reverse row the path emptied, and clears its
//! signature — no part of a dropped identity outlives the path.
//!
//! The session's `PathObject` is a FIELD of that one entry, not a second
//! authority: it is minted for an id this registry allocated, recorded on the
//! session's first request for the route, and dropped with the entry. A request
//! naming an id this registry does not hold is refused
//! ([`PathRegistry::canonical_path`]) rather than minting an object for a route
//! the engine cannot solve — which is what keeps one route one identity without
//! a second store that could disagree with this one about which route is which.
//!
//! `revision` is the registry's IDENTITY CLOCK: a monotonic counter bumped by
//! every `commit` (a new path is growth) and every `remove` that drops a path,
//! and by nothing else. A consumer that derives a session-wide value from the
//! registered paths — apply-time BAL relevance is the one caller today — keys
//! its materialization by it, so the derivation runs once per registration
//! burst instead of once per head, and a projection cannot be paired with a
//! revision it was not read at: the id, the pools, and the revision are read
//! under ONE engine hold. Reads (`lookup` / `get` / `paths_for` / `iter` /
//! `dedups`), `note_dedup`, `set_cap`, and `canonical_path` move no identity,
//! so they move no revision.
use ::degenbot_pools::solve::{HopType, MixedPath, MixedPoolRef};
use degenbot_substrate::session_registry::{PathIdentity, PathObject};
use hashbrown::HashMap;
use std::sync::Arc;
/// Typed refusal from `ArbitrageEngine::register_path` (PRG-4 — was a
/// bare `String`).
///
/// Moved here from `lifecycle` by ADR-045; re-exported at the old
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
    /// The canonical `(pool_id, zero_for_one)` hop sequence: the dedup key and
    /// the entry's record of which route it holds.
    pub(crate) signature: Vec<(u64, bool)>,
    /// The immutable per-hop pool refs stored in the committed entry.
    pub(crate) pool_refs: Vec<MixedPoolRef>,
}
/// One registered path, whole: the immutable hop refs the solver reads, the
/// canonical `(pool_id, zero_for_one)` sequence the dedup index is keyed by,
/// and the session's object for the route once the session asks for it.
///
/// The three are one identity, so they live and die together: a path dropped
/// without its signature would leave a re-registration deduping onto a dead id,
/// and one dropped without its object would leave a consumer naming a route the
/// engine no longer solves.
pub(crate) struct PathEntry {
    /// The immutable per-hop pool refs (`MixedPath`) the solve cycle reads.
    path: Arc<MixedPath>,
    /// The canonical hop sequence this entry was committed under — shared with
    /// the dedup index's key rather than copied.
    signature: Arc<[(u64, bool)]>,
    /// The session's canonical object for this route, minted on the session's
    /// first request and dropped with the entry. `None` until a session asks:
    /// a path registered without one costs nothing here.
    object: Option<Arc<PathObject>>,
}

impl PathEntry {
    /// The immutable hop refs the solve cycle reads.
    fn path(&self) -> &Arc<MixedPath> {
        &self.path
    }

    /// The canonical hop sequence this entry was committed under.
    fn signature(&self) -> &[(u64, bool)] {
        &self.signature
    }
}

/// Path identity (ADR-045): registered paths + reverse index + signatures +
/// id allocator + cap + dedup counter. Deliberately SHALLOW — no resolve, no
/// solve, no deps beyond the solver value types.
pub(crate) struct PathRegistry {
    /// Registered paths, keyed by the id this registry allocated. Each entry
    /// owns the path refs, its signature, and the session's object for it.
    paths: HashMap<u64, PathEntry>,
    /// Reverse index: (`hop_type`, `pool_key`) -> path ids using that pool.
    /// `Vec` instead of `HashSet` — sets are typically 1-4 entries, dedup at
    /// collection time.
    pub(crate) pool_to_paths: HashMap<(HopType, u64), Vec<u64>>,
    /// Dedup index: canonical `(pool_id, zero_for_one)` sequence -> path id.
    /// `register_path` is idempotent: re-registering the same hop sequence
    /// returns the existing `path_id` instead of allocating a new one
    ///.
    path_signatures: HashMap<Arc<[(u64, bool)]>, u64>,
    /// Auto-incrementing path id (starts at 1).
    next_path_id: u64,
    /// Registered-path capacity; `None` = unlimited.
    path_cap: Option<usize>,
    /// Dedup hits counted engine-side (PRG-4).
    path_dedups: u64,
    /// The identity clock: bumped by `commit` and by a `remove` that drops a
    /// path, and by nothing else (see the module docs). A consumer keys its
    /// derivation of a session-wide value by this, so it re-derives exactly when
    /// the registered set moved.
    revision: u64,
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
            paths: HashMap::new(),
            pool_to_paths: HashMap::new(),
            path_signatures: HashMap::new(),
            next_path_id: 1,
            path_cap: None,
            path_dedups: 0,
            revision: 0,
        }
    }
    /// The session's canonical path object for `path_id`, recording it on
    /// first sight; `None` for an id this registry does not hold.
    ///
    /// Get-or-create keyed by the id THIS registry allocated, so a route the
    /// session names twice yields the same object while the path is
    /// registered, and a re-registered route (which allocates a new id) yields
    /// a new one — the object cannot outlive the entry it names. `&mut`
    /// because recording is registration, like `commit`.
    ///
    /// An id this registry never allocated, or has already dropped, is
    /// REFUSED: an object minted for it would name a route the engine does not
    /// solve, which is the second identity this registry exists to prevent.
    ///
    /// This records the session's VIEW of a path, not a path: the id is
    /// already allocated, the dedup signature and cap are untouched, and the
    /// revision does not move.
    pub(crate) fn canonical_path(
        &mut self,
        chain_id: u64,
        path_id: u64,
        identity: &PathIdentity,
    ) -> Option<Arc<PathObject>> {
        let entry = self.paths.get_mut(&path_id)?;
        Some(Arc::clone(entry.object.get_or_insert_with(|| {
            Arc::new(PathObject::mint(chain_id, identity.clone(), path_id))
        })))
    }
    /// The existing path id for `signature`, or `None` when it is new.
    #[must_use]
    pub(crate) fn lookup(&self, signature: &[(u64, bool)]) -> Option<u64> {
        self.path_signatures.get(signature).copied()
    }
    /// Registered path refs for `path_id`.
    #[must_use]
    pub(crate) fn get(&self, path_id: u64) -> Option<&Arc<MixedPath>> {
        self.paths.get(&path_id).map(PathEntry::path)
    }
    /// The path ids that reference `key` (the hot fan-out read; shared), or
    /// `None` when no registered path uses it — a removal prunes the row it
    /// empties instead of leaving an empty one behind.
    #[must_use]
    pub(crate) fn paths_for(&self, key: &(HopType, u64)) -> Option<&[u64]> {
        self.pool_to_paths.get(key).map(Vec::as_slice)
    }
    /// Every registered path's hop refs, keyed by path id and apart from the
    /// entries that own them — the worker snapshot's source, so a solve bin
    /// reads paths without this registry's lock (one `Arc` bump per path).
    #[must_use]
    pub(crate) fn path_pools_snapshot(&self) -> HashMap<u64, Arc<MixedPath>> {
        self.paths
            .iter()
            .map(|(&path_id, entry)| (path_id, Arc::clone(entry.path())))
            .collect()
    }
    /// The number of registered paths.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.paths.len()
    }
    /// Whether the registry holds no paths.
    #[must_use]
    #[expect(dead_code)] // part of the ADR-045 registry surface; no caller yet
    pub(crate) fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
    /// Iterate the registered `(path_id, path)` pairs.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&u64, &Arc<MixedPath>)> {
        self.paths
            .iter()
            .map(|(path_id, entry)| (path_id, entry.path()))
    }
    /// Dedup hits counted engine-side.
    #[must_use]
    pub(crate) fn dedups(&self) -> u64 {
        self.path_dedups
    }
    /// The registry's identity clock (see the module docs): bumped by `commit`
    /// and by a `remove` that drops a path.
    ///
    /// A derivation over `iter()` is only valid at the revision it read in the
    /// SAME engine hold, which is why the projection helper pairs them
    /// (`arb_engine::path_info::session_paths`).
    #[must_use]
    pub(crate) fn revision(&self) -> u64 {
        self.revision
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
            let registered = self.paths.len();
            if registered >= cap {
                return Err(PathRegistrationError::RegistryFull { cap, registered });
            }
        }
        Ok(())
    }
    /// Commit a validated, freshly-resolved registration (all-or-nothing):
    /// allocate the id, insert the entry (pool refs + signature), extend the
    /// reverse index, and key the dedup index. Returns the allocated path id.
    pub(crate) fn commit(&mut self, reg: PathRegistration) -> u64 {
        let path_id = self.next_path_id;
        self.next_path_id += 1;
        for pool_ref in &reg.pool_refs {
            self.pool_to_paths
                .entry((pool_ref.hop_type, pool_ref.pool_key))
                .or_default()
                .push(path_id);
        }
        let signature: Arc<[(u64, bool)]> = reg.signature.into();
        self.path_signatures.insert(Arc::clone(&signature), path_id);
        self.paths.insert(
            path_id,
            PathEntry {
                path: Arc::new(MixedPath {
                    pools: reg.pool_refs,
                }),
                signature,
                object: None,
            },
        );
        self.revision = self.revision.saturating_add(1);
        path_id
    }
    /// Record a dedup hit (`register_path` returned an existing id).
    pub(crate) fn note_dedup(&mut self) {
        self.path_dedups += 1;
    }
    /// Deregister a path: drop the entry (refs, signature, and object
    /// together), prune every reverse row the path emptied, and clear its
    /// dedup signature (so a later re-registration is a fresh register).
    ///
    /// `None` for an id this registry does not hold: nothing was dropped, so
    /// nothing moves — not the counts and not the revision.
    pub(crate) fn remove(&mut self, path_id: u64) -> Option<Arc<MixedPath>> {
        let entry = self.paths.remove(&path_id)?;
        for pool_ref in &entry.path.pools {
            let key = (pool_ref.hop_type, pool_ref.pool_key);
            let emptied = self.pool_to_paths.get_mut(&key).is_some_and(|path_ids| {
                path_ids.retain(|id| *id != path_id);
                path_ids.is_empty()
            });
            if emptied {
                self.pool_to_paths.remove(&key);
            }
        }
        self.path_signatures.remove(entry.signature());
        self.revision = self.revision.saturating_add(1);
        Some(entry.path)
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

#[expect(clippy::expect_used)] // test setup: a refusal here is the failure
#[cfg(test)]
mod tests {
    use super::{PathRegistration, PathRegistry};
    use ::degenbot_pools::solve::MixedPoolRef;
    use degenbot_substrate::session_registry::PathIdentity;
    use std::sync::Arc;

    /// One V2 route over `pool_keys` (every hop trading `zero_for_one`).
    fn registration_over(pool_keys: &[u64]) -> PathRegistration {
        PathRegistration {
            signature: pool_keys.iter().map(|key| (*key, true)).collect(),
            pool_refs: pool_keys
                .iter()
                .map(|key| MixedPoolRef {
                    hop_type: ::degenbot_pools::solve::HopType::V2,
                    pool_key: *key,
                    zero_for_one: true,
                })
                .collect(),
        }
    }

    fn registration(pool_key: u64) -> PathRegistration {
        registration_over(&[pool_key])
    }

    fn reverse_key(pool_key: u64) -> (::degenbot_pools::solve::HopType, u64) {
        (::degenbot_pools::solve::HopType::V2, pool_key)
    }

    /// The identity clock: a commit moves it, a removal that drops a path moves
    /// it, and nothing else does — the one property a revision-keyed
    /// derivation depends on (a registry read cannot masquerade as a change,
    /// and a no-op removal cannot invalidate a materialization).
    #[test]
    fn the_revision_moves_on_commit_and_on_a_removal_that_drops_a_path() {
        let mut registry = PathRegistry::new();
        assert_eq!(registry.revision(), 0, "a fresh registry is at revision 0");

        let path_id = registry.commit(registration(1));
        assert_eq!(
            registry.revision(),
            1,
            "a commit is growth: it moves the clock"
        );

        // Reads, a dedup hit, a cap change, and a path-object mint move no
        // identity, so none of them may move the clock.
        let before = registry.revision();
        let _ = registry.get(path_id);
        let _ = registry.path_pools_snapshot();
        let _ = registry.iter().count();
        registry.note_dedup();
        registry.set_cap(Some(10));
        let _ = registry.canonical_path(1, path_id, &PathIdentity::default());
        assert_eq!(
            registry.revision(),
            before,
            "reads and non-identity writes leave the clock alone"
        );

        assert!(
            registry.remove(9_999).is_none(),
            "a removal of an unknown path drops nothing"
        );
        assert_eq!(
            registry.revision(),
            before,
            "a no-op removal leaves the clock alone"
        );

        assert!(registry.remove(path_id).is_some(), "the path is dropped");
        assert_eq!(
            registry.revision(),
            before + 1,
            "dropping a path moves the clock"
        );
    }

    /// `canonical_path` answers only for an id this registry holds: an id it
    /// never allocated, or one it has dropped, is refused instead of minting a
    /// session object for a route the engine cannot solve.
    #[test]
    fn canonical_path_refuses_an_id_the_registry_does_not_hold() {
        let mut registry = PathRegistry::new();
        assert!(
            registry
                .canonical_path(1, 7, &PathIdentity::default())
                .is_none(),
            "an id the allocator never issued has no path object"
        );
        assert_eq!(registry.len(), 0, "a refusal creates nothing");

        let path_id = registry.commit(registration(11));
        let object = registry
            .canonical_path(1, path_id, &PathIdentity::default())
            .expect("a committed path has a path object");
        assert_eq!(object.path_id(), path_id);
        assert_eq!(object.chain_id(), 1);
        assert_eq!(
            Arc::strong_count(&object),
            2,
            "the registry holds the object it handed the session"
        );

        assert!(registry.remove(path_id).is_some(), "the path is dropped");
        assert!(
            registry
                .canonical_path(1, path_id, &PathIdentity::default())
                .is_none(),
            "a dropped path has no path object"
        );
        assert_eq!(
            Arc::strong_count(&object),
            1,
            "deregistration withdrew the registry's hold on the object"
        );
    }

    /// A re-registered route is a NEW identity: the allocator never reuses an
    /// id, so a consumer holding the old object cannot keep naming a path the
    /// registry has dropped.
    #[test]
    fn re_registering_a_route_is_a_fresh_identity() {
        let mut registry = PathRegistry::new();
        let signature = [(11u64, true)];
        let first_id = registry.commit(registration(11));
        let first = registry
            .canonical_path(1, first_id, &PathIdentity::default())
            .expect("path object");
        assert!(registry.remove(first_id).is_some(), "the path is dropped");

        let second_id = registry.commit(registration(11));
        assert_ne!(second_id, first_id, "the allocator never reuses an id");
        assert_eq!(registry.lookup(&signature), Some(second_id));
        let second = registry
            .canonical_path(1, second_id, &PathIdentity::default())
            .expect("path object");
        assert_ne!(second.path_id(), first.path_id());
        assert!(
            !Arc::ptr_eq(&second, &first),
            "a re-registered route mints its own object"
        );
        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry.revision(),
            3,
            "two commits and one drop move the clock once each"
        );
    }

    /// Deregistration is complete: the path, its dedup signature, its reverse
    /// rows, and the session's object for it all go together, so nothing names
    /// a route the registry no longer holds.
    #[test]
    fn deregistering_a_path_withdraws_every_part_of_its_identity() {
        let mut registry = PathRegistry::new();
        let signature = [(11u64, true), (12, true)];
        let path_id = registry.commit(registration_over(&[11, 12]));
        assert!(registry.get(path_id).is_some());
        assert_eq!(registry.lookup(&signature), Some(path_id));
        assert_eq!(registry.paths_for(&reverse_key(11)), Some(&[path_id][..]));

        let removed = registry.remove(path_id).expect("the path is dropped");
        assert_eq!(removed.pools.len(), 2);
        assert!(registry.get(path_id).is_none());
        assert_eq!(
            registry.lookup(&signature),
            None,
            "the dedup key goes with the path"
        );
        assert_eq!(
            registry.paths_for(&reverse_key(11)),
            None,
            "the reverse row goes with the path"
        );
        assert_eq!(registry.paths_for(&reverse_key(12)), None);
        assert_eq!(registry.len(), 0);
    }

    /// A reverse row outlives the path that emptied it only until its LAST
    /// path is dropped: a sibling still using the pool keeps the row, and no
    /// empty row is left behind for a pool nothing hops on.
    #[test]
    fn a_reverse_index_row_survives_until_its_last_path_is_dropped() {
        let mut registry = PathRegistry::new();
        let shared = registry.commit(registration_over(&[11, 12]));
        let sibling = registry.commit(registration_over(&[11, 13]));
        assert_eq!(
            registry.paths_for(&reverse_key(11)),
            Some(&[shared, sibling][..])
        );

        assert!(registry.remove(shared).is_some(), "the shared path drops");
        assert_eq!(
            registry.paths_for(&reverse_key(11)),
            Some(&[sibling][..]),
            "a sibling path still uses the pool"
        );
        assert_eq!(
            registry.paths_for(&reverse_key(12)),
            None,
            "no empty stale row is left behind"
        );

        assert!(registry.remove(sibling).is_some(), "the sibling drops");
        assert_eq!(
            registry.paths_for(&reverse_key(11)),
            None,
            "the last user prunes the row"
        );
        assert!(
            registry.pool_to_paths.is_empty(),
            "no empty rows survive deregistration"
        );
    }
}
