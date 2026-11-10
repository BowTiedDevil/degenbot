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
use tracing::Instrument;

use degenbot_core::canonical_head::{CanonicalHead, CanonicalHeadSink};
use degenbot_core::{diag, op_span};
use degenbot_execution::PathInfo;

use crate::sim::evm2::delta_snapshot::{BalHeadDriver, HeadOutcome, HeadTicket, RelevanceSet};
use crate::sim::evm2::read_hub::CarryOutcome;
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
        // The head's own span: the prepare and the apply run off the pump's task,
        // so without it the transition's diagnostics would land outside any engine
        // span (ADR-043 §3) and the row would not export as a span event.
        let span = op_span!(
            domain = sim,
            "degenbot.sim.bal_head",
            anchor = head.number,
            hash = %head.hash,
            parent = %head.parent_hash
        );
        // The spawned phase is spawned even for a repeated delivery: the driver
        // — not this seam — owns "already settled" and "another caller is
        // preparing this head", and both answer before any fetch, so a repeat
        // costs a task and no RPC.
        (self.spawn)(Box::pin(
            async move {
                let outcome = driver
                    .advance_with_relevance(ticket, move || relevance.members())
                    .await;
                record_head_outcome(head, &outcome);
            }
            .instrument(span),
        ));
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

// ── The head's one telemetry row ───────────────────────────────────────────

/// The row ONE head transition leaves behind, or `None` when the transition moved
/// nothing (a repeat delivery, another caller's in-flight preparation).
///
/// # Why the fan-out's own label cannot carry this
///
/// A fan-out's selection reports a CLOSED cold label — `lineage-refused` covers
/// four different lineage refusals and `bal-unreadable` covers three different
/// BAL failures — because those labels are a metric/diagnostic contract a scrape
/// and a dashboard facet share. The head's own row carries the detail behind
/// them, on the transition that produced it:
///
/// - `refusal` names the exact [`CarryRefusal`](crate::sim::evm2::read_hub::CarryRefusal)
///   (`no-parent`, `not-consecutive`, `parent-hash-unknown`,
///   `parent-hash-mismatch`), which is what separates "the session's first head"
///   from "the pump skipped a block" from "a reorg rewind" in a live capture;
/// - `bal` names the exact [`BalSkip`](crate::sim::evm2::bal_prefetch::BalSkip)
///   (`no-anchor-hash`, `rpc`, `decode`), which separates an unsupported endpoint
///   from a malformed list;
/// - `prepare_us` is the head's BAL fetch and full decode (`DeltaReport::bal_latency`),
///   and `apply_us` is the apply transition's own wall (`DeltaReport::link_time`:
///   the parent seal, the link and the write pass, compaction excluded). Together
///   they are the "does preparation actually land before the fan-outs" evidence a
///   re-capture needs;
/// - `held` / `live` / `expired` are the installed generation's IDENTITY counts
///   after the transition (`expired = held - live`): what the chain holds, what it
///   still serves at its own anchor under the idle window, and the garbage a fold
///   will drop. Identity counts, never bytes — one entry is a value plus its
///   recency record — and a `held` count that falls only at a compaction fold is
///   exactly what this pair makes visible.
///
/// One `bal-head {...}` JSON object per transition, on the same `sim` domain as
/// the rest of the simulation telemetry: a reader treats any field it does not
/// know as optional, so this row is additive beside the existing rows. Only the
/// fields THIS transition has are present — a transition that moved nothing
/// writes no row at all, and a superseded or foreign apply has no wall to
/// report.
fn head_row(head: CanonicalHead, outcome: &HeadOutcome) -> Option<String> {
    use std::fmt::Write;

    let (verdict, report) = match outcome {
        HeadOutcome::Settled(report) => (
            // A settle that carried no parent value is the `cold` case the
            // fan-out's label reports as `lineage-refused` / `bal-unreadable` /
            // `settled`, and the row is where the cause is separable.
            if matches!(report.outcome, CarryOutcome::Cold { .. }) {
                "cold"
            } else {
                "settled"
            },
            Some(report),
        ),
        HeadOutcome::Superseded { .. } => ("superseded", None),
        HeadOutcome::Foreign { .. } => ("foreign", None),
        HeadOutcome::AlreadySettled | HeadOutcome::InFlight => return None,
    };
    let mut row = String::with_capacity(256);
    let _ = write!(
        row,
        "bal-head {{\"anchor\":{},\"hash\":\"{:#x}\",\"parent\":\"{:#x}\",\"outcome\":\"{verdict}\"",
        head.number, head.hash, head.parent_hash
    );
    if let Some(report) = report {
        match &report.outcome {
            CarryOutcome::Carried { carried_values } => {
                let _ = write!(row, ",\"chain\":\"carried\",\"carried\":{carried_values}");
            }
            CarryOutcome::AlreadyCurrent => {
                let _ = write!(row, ",\"chain\":\"already-current\"");
            }
            CarryOutcome::Cold { lineage, bal } => {
                let _ = write!(row, ",\"chain\":\"cold\"");
                match lineage {
                    Some(refusal) => {
                        let _ = write!(row, ",\"refusal\":\"{}\"", refusal.label());
                    }
                    None => row.push_str(",\"refusal\":null"),
                }
                match bal {
                    Some(skip) => {
                        let _ = write!(row, ",\"bal\":\"{}\"", skip.label());
                    }
                    None => row.push_str(",\"bal\":null"),
                }
            }
        }
        let _ = write!(
            row,
            ",\"prepare_us\":{},\"apply_us\":{},\"writes\":{},\"overwritten\":{},\"skipped\":{}",
            report.bal_latency.as_micros(),
            report.link_time.as_micros(),
            report.writes.len(),
            report.writes_overwritten,
            report.writes_skipped
        );
        let _ = write!(
            row,
            ",\"held\":{},\"live\":{},\"expired\":{}",
            report.held,
            report.live,
            report.held.saturating_sub(report.live)
        );
    }
    row.push('}');
    Some(row)
}

/// Emit the head's row (see [`head_row`]) on the `sim` domain at DEBUG — the
/// level the OFF/ON capture recipe enables.
fn record_head_outcome(head: CanonicalHead, outcome: &HeadOutcome) {
    if let Some(row) = head_row(head, outcome) {
        diag!(domain = sim, "{}", row);
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "the row assertions fail loudly on a broken fixture"
)]
mod tests {
    use super::*;
    use crate::sim::evm2::bal_prefetch::BalSkip;
    use crate::sim::evm2::delta_snapshot::{DeltaReport, HeadCold};
    use crate::sim::evm2::read_hub::CarryRefusal;
    use alloy::primitives::{Address, B256, U256};
    use std::time::Duration;

    const HEAD: CanonicalHead =
        CanonicalHead::new(26_382_799, B256::new([0xaa; 32]), B256::new([0x99; 32]));

    /// A report with the walls and write counts a real transition carries.
    fn report(outcome: CarryOutcome) -> Arc<DeltaReport> {
        Arc::new(DeltaReport {
            outcome,
            writes: vec![(Address::ZERO, U256::ZERO), (Address::ZERO, U256::ZERO)],
            writes_overwritten: 1,
            writes_skipped: 2,
            bal_latency: Duration::from_micros(700),
            link_time: Duration::from_micros(31),
            compaction: None,
            held: 1_546,
            live: 1_192,
        })
    }

    /// The head row names the lineage refusal the cold selection label collapses,
    /// on the transition that produced it, together with the prepare and apply
    /// walls — the two facts a live capture needs after the fact (gaps 2 and 4 of
    /// the design's evidence list).
    #[test]
    fn the_head_row_names_the_lineage_refusal_that_the_selection_label_collapses() {
        let cold = HeadCold::Settled {
            lineage: Some(CarryRefusal::NotConsecutive),
            bal: None,
        };
        assert_eq!(
            cold.label(),
            "lineage-refused",
            "the selection's label collapses four refusals into one"
        );
        let row = head_row(
            HEAD,
            &HeadOutcome::Settled(report(CarryOutcome::Cold {
                lineage: Some(CarryRefusal::NotConsecutive),
                bal: None,
            })),
        )
        .expect("a settled head leaves a row");

        assert!(row.starts_with("bal-head {"), "one greppable row: {row}");
        assert!(row.contains(r#""anchor":26382799"#), "{row}");
        assert!(
            row.contains(r#""hash":"0xaaaaaa"#),
            "the head's own exact identity: {row}"
        );
        assert!(row.contains(r#""parent":"0x999999"#), "{row}");
        assert!(
            row.contains(r#""outcome":"cold""#),
            "the settle was cold: {row}"
        );
        assert!(row.contains(r#""chain":"cold""#), "{row}");
        assert!(
            row.contains(r#""refusal":"not-consecutive""#),
            "the cause the label collapses: {row}"
        );
        assert!(row.contains(r#""bal":null"#), "the list decoded: {row}");
        assert!(
            row.contains(r#""prepare_us":700"#) && row.contains(r#""apply_us":31"#),
            "both walls: {row}"
        );
        assert!(
            row.contains(r#""writes":2"#)
                && row.contains(r#""overwritten":1"#)
                && row.contains(r#""skipped":2"#),
            "the write counts: {row}"
        );
        assert!(
            row.contains(r#""held":1546"#)
                && row.contains(r#""live":1192"#)
                && row.contains(r#""expired":354"#),
            "the generation's identity counts, expired derived: {row}"
        );
    }

    /// Every refusal and every skip has its own label: the collapsed labels are a
    /// dispatch contract, not the explanation.
    #[test]
    fn every_refusal_and_every_bal_skip_has_its_own_label() {
        assert_eq!(
            [
                CarryRefusal::NoParent.label(),
                CarryRefusal::NotConsecutive.label(),
                CarryRefusal::ParentHashUnknown.label(),
                CarryRefusal::ParentHashMismatch.label(),
            ],
            [
                "no-parent",
                "not-consecutive",
                "parent-hash-unknown",
                "parent-hash-mismatch",
            ]
        );
        assert_eq!(
            [
                BalSkip::NoAnchorHash.label(),
                BalSkip::Rpc("unsupported".into()).label(),
                BalSkip::Decode("short".into()).label(),
            ],
            ["no-anchor-hash", "rpc", "decode"]
        );
        let row = head_row(
            HEAD,
            &HeadOutcome::Settled(report(CarryOutcome::Cold {
                lineage: None,
                bal: Some(BalSkip::Rpc("unsupported".into())),
            })),
        )
        .expect("a settled head leaves a row");
        assert!(
            row.contains(r#""refusal":null"#),
            "the lineage matched: {row}"
        );
        assert!(row.contains(r#""bal":"rpc""#), "the skip cause: {row}");
    }

    /// A carried head's row says so and carries the parent-value count; a
    /// transition that moved nothing writes no row at all.
    #[test]
    fn a_carried_head_names_its_chain_and_a_transition_that_moved_nothing_writes_no_row() {
        let row = head_row(
            HEAD,
            &HeadOutcome::Settled(report(CarryOutcome::Carried { carried_values: 5 })),
        )
        .expect("a settled head leaves a row");
        assert!(row.contains(r#""outcome":"settled""#), "{row}");
        assert!(row.contains(r#""chain":"carried""#), "{row}");
        assert!(row.contains(r#""carried":5"#), "{row}");

        assert!(
            head_row(HEAD, &HeadOutcome::AlreadySettled).is_none(),
            "a repeat delivery moved nothing, so it reports nothing"
        );
        assert!(
            head_row(HEAD, &HeadOutcome::InFlight).is_none(),
            "another caller owns the preparation: nothing moved here"
        );
    }
}
