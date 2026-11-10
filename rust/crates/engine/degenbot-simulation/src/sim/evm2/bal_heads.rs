//! The session-scoped canonical-head BAL pipeline: every accepted head is
//! delivered, prepared and applied off the dispatch critical path.
//!
//! # Why the head clock, and not the fan-out
//!
//! A fan-out's anchor is the block its candidates were SOLVED at, which trails
//! the newest header by one epoch (occasionally two); candidate-bearing blocks
//! are structurally sparse, so a delivery driven by candidates can almost never
//! be consecutive with the generation already installed. Both facts together
//! mean the fan-out is the wrong clock for "keep the session's storage view
//! current": it fires too rarely and always a little late.
//!
//! The canonical head is the right clock. [`BalHeadPipeline`] is the consumer
//! of that clock: it implements [`CanonicalHeadSink`](degenbot_core::canonical_head::CanonicalHeadSink),
//! so the block pump's accepted-header arm hands it the exact
//! `(number, hash, parent_hash)` triple it already has, and the pipeline
//! delivers it to the session's ONE
//! [`BalHeadDriver`](super::delta_snapshot::BalHeadDriver).
//!
//! ```text
//!   accepted header (pump / strategy loop)
//!        │ on_canonical_head(head)                      ← synchronous, microseconds
//!        ▼
//!   deliver(head) → HeadTicket                        ← the head is now addressable
//!        │ spawn                                        ← NEVER awaited by the producer
//!        ▼
//!   prepare(ticket) → relevance → apply(ticket, …)     ← the BAL fetch lives here
//!                     ^ the SESSION's set, read after the fetch, before the apply
//! ```
//!
//! The apply is fenced by the driver's own ticket (`delta_snapshot`), so a
//! prepared list that lands after the head was superseded installs nothing and a
//! duplicate delivery repeats no work. A head that never becomes a fan-out
//! anchor is prepared and applied exactly like one that does: coverage is 100 %
//! of accepted heads, which is the whole point.
//!
//! # Armed, unarmed, and OFF
//!
//! A pipeline is constructed UNARMED: no driver, no provider, no request. An
//! unarmed pipeline's `on_canonical_head` is a no-op, so a session that
//! installed the sink but never armed it ("no executor was ever constructed, so
//! nothing consumes a generation") requests nothing — structurally, not behind
//! a flag check.
//!
//! Arming needs a provider, which only the batch executor's construction has, so
//! that is where it happens
//! ([`WarmCodeCacheInner::arm_bal_heads`](super::warm_code_cache::WarmCodeCacheInner::arm_bal_heads)).
//! With the `simulation.delta_dispatch` opt-in OFF there is no pipeline to arm
//! and no sink to install, so OFF remains request-free by construction.
//!
//! # Relevance: the session's set, materialized once per registry revision
//!
//! A head's final writes seed under a relevance set (§4.5 of the design): a
//! write to a key the generation already serves always overwrites, and an
//! unknown key seeds only when its address is relevant.
//!
//! The pump edge has no candidates and therefore no relevance to hand over, so
//! the set is a property of the APPLY:
//!
//! - [`HeadRelevanceSource`] is where the SESSION's set comes from — the union
//!   of every registered path's addresses and the deployment's settlement
//!   identities, derived by [`path_relevance_addresses`] over the paths the
//!   engine projects and the facts the deployment declares. [`NoRelevance`] is
//!   what a session that installs no source keeps: its generations carry the
//!   parent chain and answer everything else through the dynamic path, which
//!   costs reuse and never a value.
//! - The set is derived ONCE PER REGISTRY REVISION ([`SessionMembership`]) and
//!   reused by every head while that revision holds, so a steady session pays
//!   nothing per head. A newer revision derives once and swaps the whole
//!   `(revision, membership)` pair, so a head never seeds from a half-derived
//!   set.
//! - It is read at the LATEST point the transition allows: inside the spawned
//!   phase, after the head's list was fetched and immediately before it applies
//!   ([`BalHeadDriver::advance_with_relevance`]). A path registered while the
//!   list was in flight is therefore included in this head's seeding; a path
//!   registered after the apply seeds from the NEXT head and reads its own
//!   unknown slots dynamically until then. Neither can make a served value
//!   wrong, because relevance only ever gates SEEDING an unknown key.
//!
//! [`path_relevance_addresses`]: crate::sim::prefetch::path_relevance_addresses

use futures_util::future::BoxFuture;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use degenbot_core::canonical_head::{CanonicalHead, CanonicalHeadSink};
use degenbot_execution::PathInfo;

use crate::sim::evm2::delta_snapshot::{BalHeadDriver, HeadTicket, RelevanceSet};
use crate::sim::prefetch::{path_relevance_addresses, PrefetchFacts};
use crate::sim::ReadAnchor;

/// The session's relevance inputs at one registry revision: the registered
/// paths, projected, and the deployment facts the derivation reads.
///
/// One REVISION travels with the paths on purpose: the materialization swaps
/// `(revision, membership)` as one value, so a registry that grew between two
/// heads can never produce a half-derived set that mixes the old projection with
/// the new revision.
#[derive(Debug, Clone, Default)]
pub struct SessionRelevance {
    /// The registered-path revision these paths were projected at
    /// (`PathRegistry::revision`, bumped by every commit and every removal).
    pub revision: u64,
    /// Every registered path, projected to its encoder form. A path whose
    /// identity cannot be resolved contributes nothing (the engine drops it),
    /// which costs a possible seed and never a value.
    pub paths: Vec<PathInfo>,
    /// The deployment facts the derivation joins: the settlement identities
    /// (WETH9 + the V4 PoolManager) are inside `path_relevance_addresses`'s
    /// derivation when the session declares them, and absent when it does not.
    pub facts: PrefetchFacts,
}

/// Where a head's apply-time relevance comes from.
///
/// Resolved by the APPLIER, never by the head's producer: a head arrives on the
/// block clock, which has no candidates and therefore no read set to hand over.
///
/// The derivation itself is the session's (`path_relevance_addresses` over the
/// registered paths and the settlement identities, §4.5 of the design) and this
/// trait is the seam a session implements it behind. A session that installs no
/// source keeps [`NoRelevance`].
pub trait HeadRelevanceSource: Send + Sync + 'static {
    /// The session's relevance inputs, read at ONE registry revision.
    fn session_relevance(&self) -> SessionRelevance;
}

/// The relevance a session with no derivation carries: none.
///
/// A pipeline-settled head then seeds nothing of its own — its generation
/// carries the parent chain and answers every other identity through the
/// dynamic path — which costs reuse and never a value: relevance only ever
/// gates SEEDING an unknown key, so a miss reads the chain's current word.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRelevance;

impl HeadRelevanceSource for NoRelevance {
    fn session_relevance(&self) -> SessionRelevance {
        SessionRelevance::default()
    }
}

/// The pipeline's revision-keyed materialization of a [`HeadRelevanceSource`].
///
/// Deriving the session's membership costs a projection of every registered
/// path, and the registry only moves when a path is committed or removed. So the
/// derivation is keyed by the revision the source reported: while it holds, every
/// head reuses one `Arc<RelevanceSet>` and the apply pays nothing; a new revision
/// derives once and swaps the WHOLE `(revision, membership)` pair under one lock
/// hold (never a partially derived set).
struct SessionMembership {
    source: Arc<dyn HeadRelevanceSource>,
    materialized: Mutex<Option<(u64, Arc<RelevanceSet>)>>,
    /// Derivations performed — the white-box witness that the reuse is real
    /// (`BalHeadPipeline::relevance_derivations`).
    derivations: AtomicU64,
}

impl SessionMembership {
    /// The session's membership right now, derived at most once per revision.
    fn members(&self) -> Arc<RelevanceSet> {
        let inputs = self.source.session_relevance();
        let mut materialized = self.materialized.lock();
        if let Some((revision, members)) = materialized.as_ref() {
            if *revision == inputs.revision {
                return Arc::clone(members);
            }
        }
        let members = Arc::new(RelevanceSet::from_set(
            path_relevance_addresses(&inputs.paths, &inputs.facts)
                .into_iter()
                .collect(),
        ));
        self.derivations.fetch_add(1, Ordering::Relaxed);
        *materialized = Some((inputs.revision, Arc::clone(&members)));
        members
    }

    /// How many derivations this materialization performed.
    fn derivations(&self) -> u64 {
        self.derivations.load(Ordering::Relaxed)
    }
}

/// Where one head's off-path preparation runs.
///
/// The production spawner hands the future to the shared ambient runtime (the
/// same discipline every `spawn` in this tree follows); tests inject a queue so
/// "the pump task never awaits a BAL" is an assertion rather than a timing hope.
pub type HeadSpawn = Arc<dyn Fn(BoxFuture<'static, ()>) + Send + Sync>;

/// The production spawner: the shared ambient I/O runtime, detached.
///
/// The returned join handle is dropped on purpose — the task's only side effect
/// is the driver transition, which is fenced and idempotent, so there is nothing
/// to cancel and nothing to join at teardown.
#[must_use]
pub fn ambient_spawn() -> HeadSpawn {
    Arc::new(|future| {
        degenbot_core::runtime::get_runtime().spawn(future);
    })
}

/// The one armed shape: the driver every delivered head goes to.
struct Armed {
    driver: Arc<BalHeadDriver>,
}

/// The session's canonical-head pipeline (see the module docs).
pub struct BalHeadPipeline {
    spawn: HeadSpawn,
    /// The session's apply-time relevance, as the revision-keyed materialization
    /// every head shares. Swappable at construction time ([`Self::install_relevance`])
    /// because the cache that mints the pipeline is not the composition that
    /// knows the session's settlement identities.
    relevance: Mutex<Arc<SessionMembership>>,
    armed: Mutex<Option<Armed>>,
}

impl BalHeadPipeline {
    /// An UNARMED pipeline: it delivers nothing and requests nothing until
    /// [`Self::arm`] hands it a driver.
    #[must_use]
    pub fn unarmed() -> Arc<Self> {
        Self::with_spawn_and_relevance(ambient_spawn(), Arc::new(NoRelevance))
    }

    /// An unarmed pipeline whose preparation runs through `spawn` (the tests'
    /// deterministic seam) and whose applies seed under `relevance`.
    #[must_use]
    pub fn with_spawn_and_relevance(
        spawn: HeadSpawn,
        relevance: Arc<dyn HeadRelevanceSource>,
    ) -> Arc<Self> {
        Arc::new(Self {
            spawn,
            relevance: Mutex::new(Arc::new(SessionMembership {
                source: relevance,
                materialized: Mutex::new(None),
                derivations: AtomicU64::new(0),
            })),
            armed: Mutex::new(None),
        })
    }

    /// Install the session's apply-time relevance source on this pipeline
    /// (construction-time wiring).
    ///
    /// The cache mints the pipeline before any engine or deployment exists, and
    /// the session's relevance is a property of both — the registered paths
    /// (behind the engine's stage surface) and the settlement identities (behind
    /// the deployment composition). So the composition hands the source over
    /// here, exactly like [`Self::arm`] hands over the driver. A pipeline that is
    /// never handed one keeps [`NoRelevance`], which costs reuse and never a
    /// value.
    pub fn install_relevance(&self, source: Arc<dyn HeadRelevanceSource>) {
        *self.relevance.lock() = Arc::new(SessionMembership {
            source,
            materialized: Mutex::new(None),
            derivations: AtomicU64::new(0),
        });
    }

    /// How many times the pipeline materialized the session's relevance — the
    /// white-box witness for "once per registry revision, not once per head"
    /// (steady state: one per registration burst).
    #[must_use]
    pub fn relevance_derivations(&self) -> u64 {
        self.relevance.lock().derivations()
    }

    /// Arm the pipeline with the session's one head driver.
    ///
    /// Idempotent and first-arm-wins: the session owns one driver for its life,
    /// so a second arm cannot install a second generation space. Returns whether
    /// THIS call armed it (`false` when it was already armed).
    pub fn arm(&self, driver: Arc<BalHeadDriver>) -> bool {
        let mut armed = self.armed.lock();
        if armed.is_some() {
            return false;
        }
        *armed = Some(Armed { driver });
        true
    }

    /// Whether a driver is installed; unarmed means every head is dropped.
    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.armed.lock().is_some()
    }

    /// The armed driver, when there is one.
    #[must_use]
    pub fn driver(&self) -> Option<Arc<BalHeadDriver>> {
        self.armed
            .lock()
            .as_ref()
            .map(|armed| Arc::clone(&armed.driver))
    }

    /// Deliver `head` and hand its preparation to the spawner — the whole
    /// producer side of the edge, synchronous and RPC-free.
    ///
    /// Returns the ticket it minted (`None` while unarmed, which is the
    /// no-request posture). A duplicate delivery returns the SAME ticket, and
    /// the phase it spawns answers `AlreadySettled` or `InFlight` before any
    /// fetch — a repeat costs a task, never an RPC. The driver owns both
    /// verdicts; this seam does not second-guess them.
    #[must_use]
    pub fn deliver_head(&self, head: CanonicalHead) -> Option<HeadTicket> {
        // The guard is released before the delivery: the driver is the only
        // thing this seam needs from the armed state, and a head's delivery must
        // never run under the pipeline's own lock.
        let driver = {
            let armed = self.armed.lock();
            Arc::clone(&armed.as_ref()?.driver)
        };
        // The membership is resolved INSIDE the spawned phase, after the list is
        // prepared and before it applies (`BalHeadDriver::advance_with_relevance`):
        // the head's delivery must not derive anything, and a path registered
        // while the list is in flight belongs to this head's seeding.
        let relevance = Arc::clone(&self.relevance.lock());
        let ticket = driver.deliver(
            ReadAnchor::identified(head.number, head.hash),
            head.parent_hash,
        );
        // The spawned phase is spawned even for a repeated delivery: the driver
        // — not this seam — owns "already settled" and "another caller is
        // preparing this head", and both answer before any fetch, so a repeat
        // costs a task and no RPC.
        (self.spawn)(Box::pin(async move {
            let _ = driver
                .advance_with_relevance(ticket, move || relevance.members())
                .await;
        }));
        Some(ticket)
    }
}

impl CanonicalHeadSink for BalHeadPipeline {
    fn on_canonical_head(&self, head: CanonicalHead) {
        let _ = self.deliver_head(head);
    }
}

impl std::fmt::Debug for BalHeadPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BalHeadPipeline")
            .field("armed", &self.is_armed())
            .finish_non_exhaustive()
    }
}
