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
//! # Relevance
//!
//! A head's final writes seed under a relevance set (§4.5 of the design): a
//! write to a key the generation already serves always overwrites, and an
//! unknown key seeds only when its address is relevant. The pump edge has no
//! candidates and therefore no relevance to hand over, so the set is a property
//! of the APPLY: [`HeadRelevanceSource`] is the seam, and this card installs
//! [`NoRelevance`] — a pipeline-settled generation carries the parent chain and
//! answers everything else through the dynamic path. The session-wide
//! derivation (registered paths ∪ settlement identities) plugs into the same
//! seam without touching this lifecycle.

use alloy::primitives::Address;
use futures_util::future::BoxFuture;
use parking_lot::Mutex;
use std::sync::Arc;

use degenbot_core::canonical_head::{CanonicalHead, CanonicalHeadSink};

use crate::sim::evm2::delta_snapshot::{BalHeadDriver, HeadTicket};
use crate::sim::ReadAnchor;

/// The relevance set one head's final writes may seed under.
///
/// Resolved by the applier (never by the head's producer, which has no
/// candidates). The session-wide derivation is the relevance card's; this trait
/// is the seam it implements, and [`NoRelevance`] is what a session carries
/// until then.
pub trait HeadRelevanceSource: Send + Sync + 'static {
    /// The identities this session treats as relevant right now.
    fn relevance(&self) -> Vec<Address>;
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
    fn relevance(&self) -> Vec<Address> {
        Vec::new()
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
    relevance: Arc<dyn HeadRelevanceSource>,
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
            relevance,
            armed: Mutex::new(None),
        })
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
        let ticket = driver.deliver(
            ReadAnchor::identified(head.number, head.hash),
            head.parent_hash,
        );
        // The spawned phase is spawned even for a repeated delivery: the driver
        // — not this seam — owns "already settled" and "another caller is
        // preparing this head", and both answer before any fetch, so a repeat
        // costs a task and no RPC.
        let relevance = Arc::clone(&self.relevance);
        (self.spawn)(Box::pin(async move {
            let relevance = relevance.relevance();
            let _ = driver.advance(ticket, &relevance).await;
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
