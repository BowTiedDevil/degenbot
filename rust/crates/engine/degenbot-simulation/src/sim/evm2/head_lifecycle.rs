//! `head_lifecycle` — the asynchronous canonical-head lifecycle over one
//! [`DeltaSnapshotCache`]: the driver that records a delivered head by its exact
//! anchor, fetches and decodes that head's block access list in a separate
//! prepare phase, applies it to the snapshot in one synchronous transition, and
//! hands every caller at the head a view whose readiness says whether the head
//! settled.
//!
//! # The asynchronous canonical-head lifecycle
//!
//! [`BalHeadDriver`] wraps one cache with the transition a head stream needs. A
//! head arrives ([`BalHeadDriver::deliver`]) with its exact anchor and the parent
//! hash from its header — and NO relevance: a head comes off the block clock,
//! which has no candidates, so the set only has to exist when the head's writes
//! are APPLIED. The driver records the head and mints ONE cold generation that
//! every caller at that head reads until the apply lands. Its BAL is fetched and
//! fully decoded separately ([`PreparedBal`], [`BalHeadDriver::prepare`]) — the
//! stage that owns the RPC, so a caller runs it off its critical path — and
//! applied in one synchronous transition ([`BalHeadDriver::apply`]) that is
//! [`DeltaSnapshotCache::advance_bal`] with the fetch already done: same lineage
//! classification, same sealed parent chain, same write policy, same retention
//! and compaction transition.
//!
//! The relevance a head's unknown keys seed under is the apply's argument, and
//! [`BalHeadDriver::advance_with_relevance`] resolves it at the LATEST point the
//! transition allows: after the list landed, immediately before the apply. A
//! registration that committed during the fetch window is therefore included in
//! this head's seeding, and one that commits after the apply seeds from the next
//! head — without ever making a served value wrong, because relevance only gates
//! seeding an unknown key.
//!
//! Readiness is keyed by the WHOLE anchor, never the number. A caller at a
//! delivered head gets [`HeadReadiness::Cold`] with the reason until that exact
//! head's list applied — so an unprepared head, a list the node cannot serve, and
//! a malformed list all take the cold path instead of ever serving a parent
//! value at the wrong anchor. Once settled the head is [`HeadReadiness::Ready`],
//! or cold-and-settled when the lineage refused or the list could not be read
//! (the two ways the synchronous control also settles cold).
//!
//! Readiness is decided ONCE, by the settle that produced it. Delivering a head
//! whose anchor is already installed (a reorg back to the installed block)
//! repeats no transition and therefore keeps that generation's settle-derived
//! readiness: a head that settled cold answers cold with its exact reason after
//! the redelivery too, never `Ready`. A caller's diagnostic line and its metric
//! labels therefore never claim a carry that did not happen.
//!
//! Three fences keep a late or wrong response out of the snapshot: the prepared
//! list must carry the ticket's exact anchor ([`HeadOutcome::Foreign`]), the
//! ticket must still be the driver's head ([`HeadOutcome::Superseded`]), and the
//! transition itself re-classifies lineage against whatever the cache has
//! installed. A head that a same-height reorg, a gap, or a duplicate delivery
//! replaced therefore installs nothing, and the abandoned branch's values never
//! resurrect: the next head above the survivor is consecutive again and carries
//! only the survivor's own generation.
//!
//! [`BalHeadDriver::snapshot`] is synchronous, so a candidate loop never blocks
//! on a BAL; [`BalHeadDriver::advance`] is the claim-and-fetch form a caller
//! spawns, it tells a second concurrent caller [`HeadOutcome::InFlight`] instead
//! of duplicating the fetch, and a cancelled advance releases its claim so the
//! head prepares normally next time.
//!
//! A caller that wants the head's SETTLED answer joins that existing
//! preparation instead of falling back while it runs:
//! [`BalHeadDriver::wait_for_head`] registers on the exact `(anchor, seq)`
//! generation's current preparation attempt — while it is genuinely live — and
//! waits for that attempt's completion notification ([`HeadCompletion`]), then
//! re-reads the exact anchor. Completion is scoped to the ATTEMPT: a retry after
//! an abandoned attempt mints its own private epoch, so abandoning attempt N
//! stays terminal for N's waiters even when N+1 settles the same anchor before
//! they poll. The wait is bounded by real outcomes, never by a wall clock:
//! a settle (a provider failure settling cold included), a supersession, or
//! the preparation's abandonment — a generation no live preparation owns
//! answers [`HeadCold::Abandoned`] and the caller takes its fallback lane, so
//! nothing is ever parked on a preparation that is gone. The wait fetches
//! nothing, claims nothing, and holds no lock across it. Every terminal path
//! wakes its waiters, so no registration is stranded, and many waiters share
//! one attempt's one completion.
//!
//! Two costs are deliberate and visible. The provisional generation is cold, so
//! whatever it settles before the apply is dropped with it — a candidate that
//! reads the same identity again pays one more dynamic read, and the settled
//! generation never inherits a value it did not earn. And the driver keeps one
//! head at a time: a caller holding a view keeps the generation it was handed
//! (the sealed node is immutable), while a query for a superseded or never
//! delivered anchor mints a fresh cold generation per call.
//!
//! # Migration onto the lifecycle
//!
//! A consumer moving onto the lifecycle keeps the cache's semantics and changes
//! only WHO owns the clock. The synchronous [`DeltaSnapshotCache::advance_bal`]
//! remains the control — one awaited call that fetches, applies, and installs —
//! and [`BalHeadDriver`] is the same transition split across a head stream:
//!
//! 1. own the driver for the session instead of the cache
//!    ([`BalHeadDriver::new`] / [`BalHeadDriver::with_config`], which fixes the
//!    compaction threshold and the retention policy);
//! 2. on every canonical head, [`BalHeadDriver::deliver`] the anchor, the
//!    header's parent hash, and the relevance set the caller derived — the
//!    relevance input stays the CALLER's (production derivation is not part of
//!    this seam);
//! 3. run [`BalHeadDriver::prepare`] (or the whole [`BalHeadDriver::advance`])
//!    wherever the caller keeps its background work, and read every identity
//!    through [`BalHeadDriver::snapshot`]'s view — never through the parent
//!    generation — so a head that is not ready cannot answer;
//! 4. branch on [`HeadReadiness`]: `Ready` means the head's generation carries
//!    its chain, `Cold(reason)` means the caller is on the existing cold path
//!    and the reason is available for its own telemetry
//!    ([`BalHeadDriver::stats`] counts the views).
//!
//! Nothing else changes: the write policy, zero-write lists, same-anchor
//! idempotence, retention expiry, parent/child isolation, and the compaction
//! bound are the ones the cache already had, and the existing delta suites are
//! the evidence. Dispatch integration, production relevance derivation, and any
//! retention redesign remain separate workstreams — this seam ends at the view.
//! The live measurement of the stages is `just bal-head-lifecycle-probe`, and its
//! report is `docs/architecture/bal-head-lifecycle-result.md`.

use std::sync::Arc;

use alloy::primitives::{Address, B256};

use parking_lot::Mutex;
use tokio::sync::watch;

use degenbot_rpc::provider::AlloyProvider;

use super::bal_prefetch::BalSkip;
use super::delta_snapshot::{
    DeltaHub, DeltaReport, DeltaSnapshotCache, RelevanceSet, Retention, DEFAULT_MAX_DEPTH,
};
use super::read_hub::{CarryOutcome, CarryRefusal, PreparedBal};
use crate::sim::storage_memo::ReadAnchor;
// ── The asynchronous canonical-head lifecycle ──────────────────────────────
//
// A canonical head arrives from outside (a block subscription, a poller, a
// replay), and the BAL that makes the head's generation cheap to read is
// fetched and decoded separately, possibly after candidates are already running
// at that head. [`BalHeadDriver`] is the seam between those two clocks: it
// records the delivered head by its exact hash, hands every caller at that head
// a cold generation until the prepared list applies, and refuses any apply that
// does not name the head the driver currently holds.

/// The ticket one [`BalHeadDriver::deliver`] minted: the head identity a
/// preparation is applied against, and the delivery sequence that says whether
/// that head is still the driver's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadTicket {
    anchor: ReadAnchor,
    seq: u64,
}

impl HeadTicket {
    /// The exact-hash head this ticket names: readiness is keyed by this whole
    /// anchor, never by the number alone.
    #[must_use]
    pub const fn anchor(&self) -> ReadAnchor {
        self.anchor
    }
}

/// Why a caller at one exact head got a cold generation instead of a settled
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadCold {
    /// The head is delivered and its prepared BAL has not been applied yet:
    /// parent values are withheld rather than served at the wrong anchor. The
    /// synchronous view's answer for an unsettled head; a caller that wants
    /// the settled answer waits for it ([`BalHeadDriver::wait_for_head`]),
    /// which joins a genuinely live preparation and resolves terminal — a
    /// preparation that is gone or was never claimed answers
    /// [`Self::Abandoned`].
    Unprepared,
    /// A later canonical head superseded this one before it settled. The
    /// abandoned branch is never resurrected.
    Superseded,
    /// No head with this exact anchor was ever delivered to the driver.
    Undelivered,
    /// The head's preparation ended without applying — a dropped (cancelled)
    /// claim — or no preparation ever claimed this delivered head. The wait's
    /// terminal answer at this exact anchor: the caller takes its own fallback
    /// lane and nothing is left parked. A later explicit retry may still
    /// settle the head; NEW callers then read the settled generation, while a
    /// wait that already resolved is never retroactively changed.
    Abandoned,
    /// The head SETTLED at its exact hash and serves no parent value, for this
    /// reason.
    Settled {
        /// Why the settled parent chain was not carried; `None` when the lineage
        /// matched and the list is what stepped aside.
        lineage: Option<CarryRefusal>,
        /// Why the head's list was not applied; `None` when it decoded.
        bal: Option<BalSkip>,
    },
}

impl HeadCold {
    /// The closed-set label this cold reason reports — the one vocabulary a
    /// caller's diagnostic line and its metric labels share.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unprepared => "unprepared",
            Self::Superseded => "superseded",
            Self::Undelivered => "undelivered",
            Self::Abandoned => "abandoned",
            Self::Settled { lineage, bal } => {
                if lineage.is_some() {
                    "lineage-refused"
                } else if bal.is_some() {
                    "bal-unreadable"
                } else {
                    "settled"
                }
            }
        }
    }
}

/// Whether a caller's snapshot at one exact head is that head's settled
/// generation, or the cold path with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadReadiness {
    /// The head settled: reads at the view's hub resolve from its own
    /// generation, which carries the parent chain when the lineage matched.
    Ready,
    /// The cold path: the view's hub serves no parent value, and `reason` says
    /// why.
    Cold(HeadCold),
}

impl HeadReadiness {
    /// Whether the head settled.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Why the caller got a cold generation, when it did.
    #[must_use]
    pub const fn cold_reason(&self) -> Option<&HeadCold> {
        match self {
            Self::Ready => None,
            Self::Cold(reason) => Some(reason),
        }
    }
}

/// The snapshot one exact head answers with: the generation to read, and
/// whether that generation is the head's settled one.
pub struct HeadView {
    /// The generation to read. A settled head's own generation when
    /// [`Self::readiness`] is [`HeadReadiness::Ready`]; otherwise a cold
    /// generation pinned at the same anchor that carries nothing.
    pub hub: Arc<DeltaHub>,
    /// Whether the head settled, or why the cold path answered.
    pub readiness: HeadReadiness,
}

/// What one apply — or one whole background advance — did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadOutcome {
    /// The prepared list applied to the head it was prepared for: the
    /// generation is installed and later reads at that exact hash are ready.
    Settled(Arc<DeltaReport>),
    /// The head was already settled: nothing was installed and nothing moved.
    AlreadySettled,
    /// Another caller already owns this head's preparation: this call fetched
    /// nothing and waited for nothing.
    InFlight,
    /// The ticket names a head a later delivery superseded, so nothing was
    /// installed.
    Superseded {
        /// The head the driver holds now.
        head: ReadAnchor,
    },
    /// The prepared list was fetched at a different anchor than the ticket's
    /// head: nothing was installed, whatever the list says.
    Foreign {
        /// The anchor the list was prepared at.
        prepared: ReadAnchor,
        /// The anchor the ticket names.
        head: ReadAnchor,
    },
}

/// One preparation CLAIM ATTEMPT's terminal result: what the waiters that
/// joined that attempt are woken with.
///
/// Completion is scoped to the attempt, not to the head, because a head can be
/// claimed more than once: a dropped (cancelled) advance releases its claim and
/// a retry prepares the same anchor again. An attempt reaches exactly one of
/// these, and reaching one is what ends every wait registered on it
/// ([`BalHeadDriver::wait_for_head`]): the head settled, a later delivery
/// replaced it, or the preparation that owned the attempt ended without
/// applying. The first two re-read the exact anchor for the settled truth;
/// `Abandoned` is itself the wait's terminal answer — the attempt has no live
/// preparation left to settle it — so its waiters resolve
/// [`HeadCold::Abandoned`] instead of registering for one. Reaching one is
/// final for that attempt — first terminal wins, for every variant — so a
/// transition that arrives afterwards (the delivery that supersedes the head an
/// abandoned attempt held included) never rewrites what the attempt already
/// published. A retry publishes on its own private epoch, so it settles the head
/// for NEW callers and for its own waiters while the abandonment an earlier
/// attempt published stays that attempt's waiters' answer: a wait that already
/// resolved is never retroactively changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadCompletion {
    /// The head's own list applied to its exact anchor — carried or cold. The
    /// re-read then answers [`HeadReadiness::Ready`], or cold with the exact
    /// `Settled` reason.
    Settled,
    /// A later delivery replaced this head before it settled. The abandoned
    /// branch is never resurrected and never served.
    Superseded,
    /// The preparation that owned this head ended without applying (a
    /// cancelled advance, or a list prepared at a foreign anchor): the
    /// generation's waiters resolve [`HeadCold::Abandoned`] and take their
    /// fallback lane.
    Abandoned,
}

/// The head driver's cumulative counters — the lifecycle's own profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeadStats {
    /// Heads delivered (an idempotent repeat is not a delivery).
    pub delivered: u64,
    /// Deliveries that repeated the current head: no ticket minted, no work.
    pub repeated: u64,
    /// Delivered heads a later delivery superseded before they settled.
    pub superseded: u64,
    /// Heads this driver settled, carried or cold.
    pub settled: u64,
    /// Deliveries of a head whose anchor was already installed — a reorg back
    /// to the installed block — settled on the spot with no BAL work.
    pub already_current: u64,
    /// Apply attempts discarded: a superseded ticket or a foreign list.
    pub discarded: u64,
    /// Callers served a settled head's generation.
    pub ready_views: u64,
    /// Callers served a cold generation.
    pub cold_views: u64,
}

/// One preparation CLAIM ATTEMPT on one head — the private identity a wait is
/// registered against.
///
/// A head is delivered once but can be claimed more than once: a dropped
/// (cancelled) advance releases its claim and a later caller prepares the same
/// anchor again. Completion is therefore scoped to the attempt, not the head: a
/// fresh attempt mints its own epoch and its own completion channel, so a retry
/// publishes on ITS channel and can never rewrite an abandonment an earlier
/// attempt's waiters have not polled yet. Neither the epoch nor the channel is
/// public — the ticket's `seq` is the only sequencing a caller sees, and a retry
/// reuses it.
struct ClaimAttempt {
    /// Distinguishes same-anchor attempts. `0` is the attempt a delivery itself
    /// owns, which no caller has taken; every taken attempt mints a larger
    /// epoch, and the epoch is what keeps a released attempt's publication off
    /// a later attempt's channel.
    epoch: u64,
    /// This attempt's terminal result, `None` until the attempt ends: the one
    /// channel every waiter that joined THIS attempt is woken on.
    completion: watch::Sender<Option<HeadCompletion>>,
}

impl ClaimAttempt {
    /// The attempt a delivery owns: no caller has taken it and nothing is
    /// published.
    fn delivered() -> Self {
        Self::undecided(0)
    }

    /// A fresh attempt at `epoch`: its own channel, so the attempt's terminal
    /// result is its own.
    fn minted(epoch: u64) -> Self {
        Self::undecided(epoch)
    }

    /// The attempt a head's settled truth is published on when the attempt that
    /// owned the head already ended: its terminal result is published at
    /// creation, because nothing is left to prepare.
    fn settled(epoch: u64) -> Self {
        let (completion, _) = watch::channel(Some(HeadCompletion::Settled));
        Self { epoch, completion }
    }

    /// An attempt nothing has decided yet — the shape a delivery and a fresh
    /// claim attempt share.
    fn undecided(epoch: u64) -> Self {
        let (completion, _) = watch::channel(None);
        Self { epoch, completion }
    }

    /// The terminal result this attempt published, if it has one.
    fn published(&self) -> Option<HeadCompletion> {
        *self.completion.borrow()
    }

    /// Publish this attempt's terminal result — FIRST TERMINAL WINS, for every
    /// variant.
    ///
    /// One attempt reaches exactly one terminal answer, and the first one
    /// published is what the waiters that joined the attempt are woken with. A
    /// later publication for the SAME attempt is dropped, whichever variant it
    /// carries: a settle is not rewritten by the claim's release, a released
    /// attempt's abandonment is not rewritten by the delivery that supersedes
    /// the head it held, and a supersession is not rewritten by the release
    /// that follows it. Nothing needs to republish over its own attempt — a
    /// retry mints a new attempt with its own epoch and channel and publishes
    /// there, so a later attempt's real terminal never depends on overwriting
    /// an earlier attempt's.
    fn publish(&self, completion: HeadCompletion) {
        if self.completion.borrow().is_none() {
            self.completion.send_replace(Some(completion));
        }
    }

    /// This attempt's completion channel, for a waiter joining a genuinely live
    /// preparation.
    fn subscribe(&self) -> watch::Receiver<Option<HeadCompletion>> {
        self.completion.subscribe()
    }
}

/// The delivery identity every pose of the current head carries: the exact-hash
/// anchor, the parent hash its lineage is verified against, and the public
/// delivery sequence its ticket names.
struct HeadDelivery {
    anchor: ReadAnchor,
    parent_hash: B256,
    seq: u64,
}

/// A delivered head that has not settled: the ONE cold generation every caller
/// at it shares until its BAL applies, and the claim attempt that owns — or,
/// once released, owned — its preparation.
struct UnsettledHead {
    delivery: HeadDelivery,
    /// The cold generation callers read while the head is unprepared, dropped at
    /// settle so the settled generation replaces it wholesale.
    provisional: Arc<DeltaHub>,
    attempt: ClaimAttempt,
}

/// A settled head: from here the anchor's generation is the cache's, so the cold
/// generation is gone. `report` is the transition this driver ran, absent when a
/// delivery found the generation already installed.
struct SettledHead {
    delivery: HeadDelivery,
    report: Option<Arc<DeltaReport>>,
    attempt: ClaimAttempt,
}

/// The head the driver holds, in exactly one pose.
///
/// The pose is what the payload combinations make type-safe: an unsettled head
/// always carries the cold generation its callers share and a settled one never
/// does, and "a caller owns the preparation" is a pose rather than a flag — so
/// the liveness a wait registers on and the data the state carries cannot
/// disagree.
enum CurrentHead {
    /// Delivered with nothing owning its preparation: a direct `prepare` +
    /// `apply` moves it, and a released (cancelled) attempt leaves it here with
    /// that attempt's own terminal result already published.
    Unclaimed(UnsettledHead),
    /// A live claim attempt owns the preparation: the state a wait registers on
    /// and a second concurrent advance joins as `InFlight`.
    Preparing(UnsettledHead),
    /// The head settled, carried or cold.
    Settled(SettledHead),
}

impl CurrentHead {
    fn delivery(&self) -> &HeadDelivery {
        match self {
            Self::Unclaimed(head) | Self::Preparing(head) => &head.delivery,
            Self::Settled(head) => &head.delivery,
        }
    }

    fn anchor(&self) -> ReadAnchor {
        self.delivery().anchor
    }

    fn attempt(&self) -> &ClaimAttempt {
        match self {
            Self::Unclaimed(head) | Self::Preparing(head) => &head.attempt,
            Self::Settled(head) => &head.attempt,
        }
    }

    /// The cold generation the head shares with its callers, while it has one.
    fn provisional(&self) -> Option<&Arc<DeltaHub>> {
        match self {
            Self::Unclaimed(head) | Self::Preparing(head) => Some(&head.provisional),
            Self::Settled(_) => None,
        }
    }

    /// The report this driver's apply produced, once the head settled.
    fn report(&self) -> Option<Arc<DeltaReport>> {
        match self {
            Self::Settled(head) => head.report.as_ref().map(Arc::clone),
            Self::Unclaimed(_) | Self::Preparing(_) => None,
        }
    }

    /// Whether the head's current attempt already published its own end, so a
    /// settle must mint a fresh private epoch instead of rewriting it.
    fn published_abandonment(&self) -> bool {
        matches!(
            self,
            Self::Unclaimed(head)
                if matches!(head.attempt.published(), Some(HeadCompletion::Abandoned))
        )
    }

    /// Take the live claim `epoch` on an unclaimed head: the pose a caller owns
    /// the preparation in.
    fn claimed(self, epoch: u64) -> Self {
        match self {
            Self::Unclaimed(head) => Self::Preparing(UnsettledHead {
                attempt: ClaimAttempt::minted(epoch),
                ..head
            }),
            other => other,
        }
    }

    /// Release the live claim attempt `epoch` on the head `seq` delivered —
    /// publishing its own end — and leave the pose unclaimed, so a later caller
    /// prepares the same head normally. Any other pose is untouched: a settle or
    /// a supersession already decided that attempt's waiters' answer, and a
    /// release never rewrites it.
    fn released(self, seq: u64, epoch: u64) -> Self {
        match self {
            Self::Preparing(head) if head.delivery.seq == seq && head.attempt.epoch == epoch => {
                head.attempt.publish(HeadCompletion::Abandoned);
                Self::Unclaimed(head)
            }
            other => other,
        }
    }

    /// Publish the settle on this pose's attempt: the live attempt's own channel,
    /// the delivery's undecided one, or — when the attempt that held the head
    /// already published its own end — the `fresh` private epoch.
    fn publish_settle(&mut self, fresh: Option<u64>) {
        match self {
            Self::Unclaimed(head) => match fresh {
                Some(epoch) => head.attempt = ClaimAttempt::settled(epoch),
                None => head.attempt.publish(HeadCompletion::Settled),
            },
            Self::Preparing(head) => head.attempt.publish(HeadCompletion::Settled),
            Self::Settled(head) => head.attempt.publish(HeadCompletion::Settled),
        }
    }

    /// The settled pose, holding the report of the transition that settled it.
    ///
    /// A settle ALWAYS stores the report it just produced: the settled pose never
    /// keeps an older transition's report, whichever pose the settle found. `None`
    /// belongs to the delivery that found its generation already installed — it
    /// runs no transition, so it passes through no settle and can never overwrite
    /// the report of one that did.
    fn settled(self, report: Arc<DeltaReport>) -> Self {
        match self {
            Self::Settled(head) => Self::Settled(SettledHead {
                report: Some(report),
                ..head
            }),
            Self::Unclaimed(head) | Self::Preparing(head) => Self::Settled(SettledHead {
                delivery: head.delivery,
                report: Some(report),
                attempt: head.attempt,
            }),
        }
    }
}

/// The installed generation: the anchor the cache has installed and the readiness
/// its OWN settle derived. The cache owns the hub; the lifecycle owns why it
/// carries — one settle-derived readiness, never re-derived from a hub.
struct InstalledGeneration {
    anchor: ReadAnchor,
    readiness: HeadReadiness,
}

/// The one previous anchor a lagging caller can still read: the generation the
/// installing transition replaced, with the handle that was installed there and
/// the readiness it reported.
///
/// Set only by a transition that linked (the new head was exactly the previous
/// anchor + 1 with a matching parent hash), and cleared by any settle that did
/// not, so a gap makes the window smaller, never wrong. This is a window, not
/// retention: the handle is the generation the seal already folded into the new
/// head's node, so it costs one `Arc` and an empty handle-local map — not one
/// snapshot.
struct PreviousGeneration {
    anchor: ReadAnchor,
    hub: Arc<DeltaHub>,
    readiness: HeadReadiness,
}

/// What one snapshot reads at an anchor the cache has installed NO generation
/// at, decided by the lifecycle under its lock.
///
/// The installed generation is deliberately not one of the answers: a read asks
/// the cache for it first ([`DeltaSnapshotCache::installed_hub`]) and reads the
/// handle that lookup returns. No lifecycle verdict can therefore stand in for
/// an installed generation, and there is no pose from which a read could mint
/// one.
enum Lookup {
    /// The current head is unprepared: every caller at it shares this one cold
    /// generation.
    Unprepared { hub: Arc<DeltaHub> },
    /// The anchor outside the installed head that the window still covers.
    Previous {
        hub: Arc<DeltaHub>,
        readiness: HeadReadiness,
    },
    /// Nothing the driver holds answers at this anchor.
    Cold { reason: HeadCold },
}

/// What one claim attempt's prologue decided.
enum ClaimStart {
    /// The head is this caller's to prepare now, on this private epoch.
    Claimed(u64),
    /// Nothing was claimed; this is the answer the advance returns.
    Refused(HeadOutcome),
}

/// What one apply decided before the cache is touched.
enum ApplyPlan {
    /// Nothing is installed; this is the answer the apply returns.
    Refused(HeadOutcome),
    /// Install `anchor`'s prepared list over the verified parent lineage.
    Admitted {
        anchor: ReadAnchor,
        parent_hash: B256,
        /// The installed anchor the transition seals over, when there is one.
        prior: Option<ReadAnchor>,
    },
}

/// The BAL head LIFECYCLE: the driver's one authority over a stream of canonical
/// heads.
///
/// # The three poses
///
/// The head the driver holds — `current` — is in exactly one of
/// [`CurrentHead::Unclaimed`], [`CurrentHead::Preparing`] or
/// [`CurrentHead::Settled`]. Unclaimed and Preparing carry the delivery identity,
/// the one cold generation their callers share and the claim attempt a wait
/// registers against; Settled carries the delivery identity, the report of the
/// transition that settled it and the attempt its waiters were woken on. A
/// cancelled preparation returns the head to Unclaimed — the same head, not a
/// new pose — and a spawned phase's release stays a spawned-phase concept, never
/// a driver pose.
///
/// # The projections
///
/// Everything else a caller can observe is an orthogonal projection of that pose
/// plus the generation history: [`InstalledGeneration`] (the readiness the
/// installed head's own settle derived), [`PreviousGeneration`] (the one anchor
/// the read window still covers), the superseded anchor (the one abandoned head a
/// lagging caller learns about), the per-attempt completion, the public ticket
/// sequence and the counters. Cold is a readiness value, abandonment is one
/// attempt's terminal result, and none of them is a second lifecycle authority:
/// the cache installs hubs, this machine is the only lifecycle writer.
#[derive(Default)]
struct HeadLifecycle {
    current: Option<CurrentHead>,
    /// The anchor the last delivery superseded: one slot, so a caller still
    /// working at the abandoned head learns why its snapshot is cold.
    superseded: Option<ReadAnchor>,
    /// The installed generation's readiness, kept after its head is superseded so
    /// a caller still at that head gets the generation — and the truth about
    /// whether it carries — rather than a fresh cold one.
    installed: Option<InstalledGeneration>,
    previous: Option<PreviousGeneration>,
    seq: u64,
    /// The epoch the next preparation claim attempt mints. Private: public
    /// sequencing is the delivery `seq`, which a retry reuses.
    claim_epoch: u64,
    stats: HeadStats,
}

impl HeadLifecycle {
    /// The readiness the `anchor`'s own settle derived; a generation the machine
    /// holds no settle for is the ready one.
    fn readiness_of(&self, anchor: ReadAnchor) -> HeadReadiness {
        self.installed
            .as_ref()
            .filter(|generation| generation.anchor == anchor)
            .map_or(HeadReadiness::Ready, |generation| {
                generation.readiness.clone()
            })
    }

    /// Deliver one head: mint the public delivery sequence, retire the head it
    /// replaces, and put the new head in the pose its installation calls for.
    fn deliver(
        &mut self,
        anchor: ReadAnchor,
        parent_hash: B256,
        installed: Option<ReadAnchor>,
        cold: impl FnOnce() -> Arc<DeltaHub>,
    ) -> u64 {
        if let Some(seq) = self
            .current
            .as_ref()
            .map(CurrentHead::delivery)
            .filter(|held| held.anchor == anchor && held.parent_hash == parent_hash)
            .map(|held| held.seq)
        {
            self.stats.repeated = self.stats.repeated.saturating_add(1);
            return seq;
        }
        self.seq = self.seq.saturating_add(1);
        let seq = self.seq;
        if let Some(replaced) = self.current.take() {
            if replaced.provisional().is_some() {
                self.stats.superseded = self.stats.superseded.saturating_add(1);
            }
            // The replaced generation is terminal for its waiters: the wake is
            // what makes a stale registration resolve instead of sleeping through
            // its own abandonment. An attempt that already published its own end
            // keeps it — this supersession is the HEAD's transition, not a second
            // terminal for that attempt.
            replaced.attempt().publish(HeadCompletion::Superseded);
            self.superseded = Some(replaced.anchor());
        }
        self.stats.delivered = self.stats.delivered.saturating_add(1);
        let delivery = HeadDelivery {
            anchor,
            parent_hash,
            seq,
        };
        self.current = Some(if installed == Some(anchor) {
            self.stats.already_current = self.stats.already_current.saturating_add(1);
            // The readiness the installed generation's own settle derived — the
            // same record `lookup` reads — so a redelivery restates no transition
            // it did not perform.
            let readiness = self.readiness_of(anchor);
            self.installed = Some(InstalledGeneration { anchor, readiness });
            // A delivery of an installed block settles on the spot: its generation
            // is terminal the moment it exists.
            CurrentHead::Settled(SettledHead {
                delivery,
                report: None,
                attempt: ClaimAttempt::settled(0),
            })
        } else {
            CurrentHead::Unclaimed(UnsettledHead {
                delivery,
                provisional: cold(),
                attempt: ClaimAttempt::delivered(),
            })
        });
        seq
    }

    /// Take the live preparation claim on the ticket's head.
    ///
    /// Only one caller prepares a given head: a second concurrent advance on the
    /// same ticket is `InFlight`, an installed or settled head is
    /// `AlreadySettled`, and a ticket a later delivery superseded installs
    /// nothing. A fresh attempt on an unclaimed head mints its own private epoch
    /// and channel — its terminal result can never overwrite the one the attempt
    /// it replaces published — while the public ticket is untouched.
    fn claim(&mut self, anchor: ReadAnchor, seq: u64, installed: Option<ReadAnchor>) -> ClaimStart {
        let Some(current) = self.current.as_ref() else {
            return ClaimStart::Refused(HeadOutcome::Superseded { head: anchor });
        };
        let held = current.delivery();
        if held.seq != seq || held.anchor != anchor {
            return ClaimStart::Refused(HeadOutcome::Superseded { head: held.anchor });
        }
        if installed == Some(anchor) {
            return ClaimStart::Refused(HeadOutcome::AlreadySettled);
        }
        match current {
            CurrentHead::Preparing(_) => return ClaimStart::Refused(HeadOutcome::InFlight),
            // A settled head the cache no longer holds has nothing left to
            // prepare: the answer is the one an installed head gives.
            CurrentHead::Settled(_) => return ClaimStart::Refused(HeadOutcome::AlreadySettled),
            CurrentHead::Unclaimed(_) => {}
        }
        self.claim_epoch = self.claim_epoch.saturating_add(1);
        let epoch = self.claim_epoch;
        self.current = self.current.take().map(|current| current.claimed(epoch));
        ClaimStart::Claimed(epoch)
    }

    /// Release the live claim attempt a dropped preparation owned.
    fn release(&mut self, seq: u64, epoch: u64) {
        if let Some(head) = self.current.take() {
            self.current = Some(head.released(seq, epoch));
        }
    }

    /// Decide one apply under the lock: the exact-hash fence, the foreign-list
    /// fence, the installed head's idempotence, and which generation the
    /// transition seals over. A refusal is counted here and installs nothing.
    fn plan_apply(
        &mut self,
        anchor: ReadAnchor,
        seq: u64,
        prepared: ReadAnchor,
        installed: Option<ReadAnchor>,
    ) -> ApplyPlan {
        let Some(current) = self.current.as_ref() else {
            self.stats.discarded = self.stats.discarded.saturating_add(1);
            return ApplyPlan::Refused(HeadOutcome::Superseded { head: anchor });
        };
        let (held_anchor, held_seq, parent_hash) = {
            let held = current.delivery();
            (held.anchor, held.seq, held.parent_hash)
        };
        if held_seq != seq || held_anchor != anchor {
            self.stats.discarded = self.stats.discarded.saturating_add(1);
            return ApplyPlan::Refused(HeadOutcome::Superseded { head: held_anchor });
        }
        if prepared != held_anchor {
            self.stats.discarded = self.stats.discarded.saturating_add(1);
            return ApplyPlan::Refused(HeadOutcome::Foreign {
                prepared,
                head: held_anchor,
            });
        }
        if installed == Some(held_anchor) {
            // The head is already installed — this apply found it settled: its
            // generation is terminal and its waiters wake on the head's current
            // attempt.
            self.publish_settle();
            return ApplyPlan::Refused(HeadOutcome::AlreadySettled);
        }
        ApplyPlan::Admitted {
            anchor: held_anchor,
            parent_hash,
            prior: installed,
        }
    }

    /// Record one installed generation: the counters, the read window, and the
    /// readiness the transition derived from its own report.
    fn record_settle(
        &mut self,
        anchor: ReadAnchor,
        report: &Arc<DeltaReport>,
        prior: Option<(ReadAnchor, Arc<DeltaHub>)>,
    ) {
        self.stats.settled = self.stats.settled.saturating_add(1);
        // The two-anchor window: a linked transition makes the generation it just
        // sealed the previous anchor a lagging caller may read. A settle that did
        // not link (no parent, not consecutive, a mismatched parent hash, or an
        // unreadable list) leaves no previous slot, so the window shrinks across a
        // gap instead of pointing at a generation that never was this chain's
        // parent.
        match &report.outcome {
            CarryOutcome::Carried { .. } => {
                if let Some((prior_anchor, prior_hub)) = prior {
                    let readiness = self.readiness_of(prior_anchor);
                    self.previous = Some(PreviousGeneration {
                        anchor: prior_anchor,
                        hub: prior_hub,
                        readiness,
                    });
                }
            }
            CarryOutcome::Cold { .. } => self.previous = None,
            CarryOutcome::AlreadyCurrent => {}
        }
        self.installed = Some(InstalledGeneration {
            anchor,
            readiness: settled_readiness(Some(report.as_ref())),
        });
    }

    /// Publish the settle and move the current head to its settled pose — the
    /// cold generation dropped, the report and the attempt kept.
    ///
    /// A head can settle while its current attempt already published its own end:
    /// an out-of-band [`BalHeadDriver::apply`] on the head's ticket can land after
    /// the attempt that claimed the head was dropped, before any retry claims it.
    /// That attempt published `Abandoned`, and an attempt's terminal result is
    /// never rewritten, so [`Self::publish_settle`] mints a fresh private epoch
    /// and channel for the settle. Only the private epoch moves — the public
    /// ticket's `seq` is untouched, so no caller sees a new generation.
    fn settle_current(&mut self, report: Arc<DeltaReport>) {
        self.publish_settle();
        if let Some(current) = self.current.take() {
            self.current = Some(current.settled(report));
        }
    }

    /// Publish the head's settled truth on its current attempt without moving the
    /// pose (an apply that found the generation already installed).
    ///
    /// An ordinary live-claim settle, a delivery's undecided attempt, and a head
    /// that is already settled all publish on the attempt they already have:
    /// nothing was abandoned, so no new epoch is minted.
    fn publish_settle(&mut self) {
        let fresh = if self
            .current
            .as_ref()
            .is_some_and(CurrentHead::published_abandonment)
        {
            self.claim_epoch = self.claim_epoch.saturating_add(1);
            Some(self.claim_epoch)
        } else {
            None
        };
        if let Some(current) = self.current.as_mut() {
            current.publish_settle(fresh);
        }
    }

    /// What one snapshot at `anchor` reads from the pose and the history alone,
    /// for an anchor the cache has installed no generation at.
    ///
    /// A SETTLED head the cache does not hold installed is not an installed
    /// generation: the cache owns one installed generation and it is at another
    /// anchor, so this anchor's values went with it. The caller reads cold — the
    /// one answer that neither serves another anchor's values nor installs the
    /// generation back to make the pose look consistent again.
    fn lookup(&self, anchor: ReadAnchor) -> Lookup {
        match self.current.as_ref().filter(|head| head.anchor() == anchor) {
            Some(CurrentHead::Unclaimed(head) | CurrentHead::Preparing(head)) => {
                Lookup::Unprepared {
                    hub: Arc::clone(&head.provisional),
                }
            }
            Some(CurrentHead::Settled(_)) | None => match self
                .previous
                .as_ref()
                .filter(|previous| previous.anchor == anchor)
            {
                Some(previous) => Lookup::Previous {
                    hub: Arc::clone(&previous.hub),
                    readiness: previous.readiness.clone(),
                },
                None => Lookup::Cold {
                    reason: if self.superseded == Some(anchor) {
                        HeadCold::Superseded
                    } else {
                        HeadCold::Undelivered
                    },
                },
            },
        }
    }

    /// Count one served view on whichever counter its readiness belongs to.
    fn record_view(&mut self, ready: bool) {
        if ready {
            self.stats.ready_views = self.stats.ready_views.saturating_add(1);
        } else {
            self.stats.cold_views = self.stats.cold_views.saturating_add(1);
        }
    }

    /// The wait registration for `anchor`, under the state lock: [`Option::None`]
    /// when no LIVE preparation owns the exact anchor's unsettled generation — it
    /// settled, it is not the driver's current head any more, or nothing has
    /// claimed it — otherwise the live attempt's own completion signal.
    ///
    /// Subscribing is reserved for a genuinely live attempt (the `Preparing`
    /// pose): a delivered head nothing claimed yet, and a head whose claim was
    /// dropped, have no future notification worth waiting for, so the wait
    /// answers terminal instead of parking.
    fn registration(
        &self,
        anchor: ReadAnchor,
        installed: Option<ReadAnchor>,
    ) -> Option<watch::Receiver<Option<HeadCompletion>>> {
        if installed == Some(anchor) {
            return None;
        }
        match self.current.as_ref() {
            Some(CurrentHead::Preparing(head)) if head.delivery.anchor == anchor => {
                Some(head.attempt.subscribe())
            }
            _ => None,
        }
    }

    /// The terminal result the CURRENT attempt on `anchor` published, when the
    /// head the driver holds is that one.
    fn completion(&self, anchor: ReadAnchor) -> Option<HeadCompletion> {
        let current = self.current.as_ref()?;
        if current.anchor() != anchor {
            return None;
        }
        current.attempt().published()
    }

    /// The head the driver holds — settled or still preparing.
    fn delivered(&self) -> Option<ReadAnchor> {
        self.current.as_ref().map(CurrentHead::anchor)
    }

    /// The previous anchor the read window covers, when the installing transition
    /// linked.
    fn previous_anchor(&self) -> Option<ReadAnchor> {
        self.previous.as_ref().map(|previous| previous.anchor)
    }

    /// The report this driver's own apply produced for the head it holds.
    fn last_report(&self) -> Option<Arc<DeltaReport>> {
        self.current.as_ref().and_then(CurrentHead::report)
    }

    /// The lifecycle's cumulative counters.
    fn stats(&self) -> HeadStats {
        self.stats
    }
}

/// The asynchronous canonical-head lifecycle for one [`DeltaSnapshotCache`].
///
/// # Prepare, then apply
///
/// [`Self::deliver`] records a head by its exact anchor and mints the cold
/// generation callers read until the apply lands. [`Self::prepare`] fetches and
/// fully decodes that head's BAL — the stage that owns the RPC, so the caller
/// runs it off its critical path — and [`Self::apply`] moves the snapshot in one
/// synchronous transition: the exact lineage classification against the
/// installed generation, the sealed parent chain linked only when the lineage
/// matched and the list decoded, the final writes applied under the caller's
/// relevance set, and the retention/compaction transition of
/// [`DeltaSnapshotCache::advance_bal`] unchanged. [`Self::advance`] is the two
/// stages together, for a caller that owns the whole background phase.
///
/// # What a caller is guaranteed
///
/// - A view at a delivered head is [`HeadReadiness::Cold`] — serving no parent
///   value — until that exact head's prepared list applied. An unsupported,
///   missing, or malformed list is the cold path with the reason
///   reported, never a stale parent value.
/// - A prepared list applies only to the head it was prepared for: the exact
///   anchor is fenced, and a list whose head a later delivery superseded is
///   discarded, so a late response can never install an abandoned branch.
/// - A settled generation is installed under the same exact-hash lineage rule as
///   the synchronous control, so the only ways a head settles cold are the ones
///   `advance_bal` already has (a lineage refusal, an unreadable list, or both).
/// - Reads are never blocked by a pending preparation: [`Self::snapshot`] is
///   synchronous, and a caller that wants to wait owns the await on the
///   preparation itself.
///
/// # Late settles, single flight, and what the cold generation costs
///
/// A caller that already holds a view keeps reading the generation it was handed
/// when the head moves on: the sealed node is immutable, so a settle that lands
/// after the apply stays in the handle that asked (the delta chain's existing
/// late-settle locality). While a head is unprepared every caller at it shares
/// ONE cold generation, so duplicate reads coalesce and distinct misses share
/// one storage batch exactly as they do at any anchored hub; when the head
/// settles, that provisional generation is dropped and values it settled are
/// not carried into the settled one — a candidate that reads the same identity
/// again pays a second dynamic read, which is the price of the cold path, not a
/// correctness question, and a query for that same head after the next one
/// arrived still answers with the generation that settled it. A view at an
/// anchor no head ever answered (never delivered, or an abandoned head) mints a
/// fresh cold generation per query: hold the earlier view for a stable hub.
pub struct BalHeadDriver {
    provider: AlloyProvider,
    cache: DeltaSnapshotCache,
    retention: Retention,
    state: Mutex<HeadLifecycle>,
}

impl BalHeadDriver {
    /// A driver over a fresh cache at [`DEFAULT_MAX_DEPTH`] and
    /// [`Retention::Unbounded`].
    #[must_use]
    pub fn new(provider: &AlloyProvider) -> Self {
        Self::with_config(provider, DEFAULT_MAX_DEPTH, Retention::Unbounded)
    }

    /// A driver whose cache uses `max_depth` and `retention` for every
    /// generation it installs, and whose provisional generations match.
    #[must_use]
    pub fn with_config(provider: &AlloyProvider, max_depth: u32, retention: Retention) -> Self {
        Self {
            provider: provider.clone(),
            cache: DeltaSnapshotCache::with_max_depth_and_retention(provider, max_depth, retention),
            retention,
            state: Mutex::new(HeadLifecycle::default()),
        }
    }

    /// The retention policy every generation this driver installs obeys.
    ///
    /// A reader surface, not a knob: the policy is chosen once at construction
    /// ([`Self::new`] is the library default, [`Self::with_config`] is a
    /// caller's choice), so the selection a session made is checkable without
    /// reproducing a transition.
    #[must_use]
    pub const fn retention(&self) -> Retention {
        self.retention
    }

    /// The compaction threshold every generation this driver installs obeys
    /// (`DEFAULT_MAX_DEPTH` unless the caller passed another).
    ///
    /// The fold horizon is what bounds expired-but-undropped garbage, so the
    /// threshold is part of the retention answer rather than a separate
    /// tuning.
    #[must_use]
    pub const fn max_depth(&self) -> u32 {
        self.cache.max_depth
    }

    /// Deliver one canonical head: its exact anchor and the parent hash its
    /// lineage is verified against.
    ///
    /// Relevance is NOT a delivery property: a head arrives on the block clock,
    /// which has no candidates and therefore no relevance to hand over, and the
    /// set only needs to exist by the time the head's final writes are applied.
    /// It is an argument of [`Self::apply`] for that reason — the applier is
    /// whoever knows the session's relevant identities.
    ///
    /// A delivery identical to the current head — same anchor and same parent
    /// hash — is idempotent: the same ticket comes back and nothing is minted. A
    /// delivery of a head whose anchor the cache already has installed settles on
    /// the spot without any BAL work (a reorg back to the installed block), and
    /// answers that generation with the readiness its own settle derived, so a
    /// head that settled cold stays cold with its reason instead of being
    /// restated as [`HeadReadiness::Ready`] — a redelivery repeats no transition,
    /// so it cannot restate one. Everything else minted a cold generation that
    /// callers at that head read until [`Self::apply`] lands.
    pub fn deliver(&self, anchor: ReadAnchor, parent_hash: B256) -> HeadTicket {
        let mut state = self.state.lock();
        let installed = self.cache.anchor();
        let seq = state.deliver(anchor, parent_hash, installed, || {
            cold_generation(&self.provider, anchor, self.retention)
        });
        HeadTicket { anchor, seq }
    }

    /// Fetch and fully decode `ticket`'s head BAL — the prepared phase, which
    /// moves no snapshot state. The caller owns the future, so this is what runs
    /// off the critical path; the exact-hash fence is enforced by
    /// [`Self::apply`].
    pub async fn prepare(&self, ticket: HeadTicket) -> PreparedBal {
        PreparedBal::prepare(&self.provider, ticket.anchor).await
    }

    /// Apply `prepared` to `ticket`'s head in one synchronous transition, with
    /// `relevance` naming the identities this head's writes may SEED (a write to
    /// a key the generation already serves always overwrites; an unknown key
    /// seeds only when its address is relevant).
    ///
    /// The list applies only when the ticket names the head the driver still
    /// holds and the list was prepared at that same exact anchor; a superseded
    /// ticket and a foreign list install nothing. A head that is already settled
    /// is idempotent: nothing moves and no write is re-applied.
    ///
    /// A successful apply settles the head's CURRENT claim attempt, which is what
    /// [`Self::completion`] and a new [`Self::wait_for_head`] report: a live
    /// attempt publishes its own settle, while a head whose attempt was dropped
    /// (cancelled) before it settled — no retry holding the head, so an apply can
    /// land on a head whose only claim already ended — publishes the settle on a
    /// FRESH private attempt epoch, leaving that abandonment as its own waiters'
    /// answer. The replacement is not public sequencing: the ticket's `seq` is
    /// unchanged and no caller sees a new generation.
    pub fn apply(
        &self,
        ticket: HeadTicket,
        prepared: &PreparedBal,
        relevance: &[Address],
    ) -> HeadOutcome {
        self.apply_members(ticket, prepared, &RelevanceSet::from_slice(relevance))
    }

    /// The transition with the relevance as a membership set the caller already
    /// holds: the session's apply-time relevance is materialized once per
    /// registry revision, so the head pipeline hands the set over and the apply
    /// pays no conversion. [`Self::apply`] is this method with a caller's slice
    /// converted first.
    ///
    /// Membership is the whole difference between the two, and it moves no
    /// decision (pinned by
    /// `the_set_and_slice_relevance_inputs_produce_an_identical_report`).
    pub fn apply_members(
        &self,
        ticket: HeadTicket,
        prepared: &PreparedBal,
        relevance: &RelevanceSet,
    ) -> HeadOutcome {
        let mut state = self.state.lock();
        let plan = state.plan_apply(
            ticket.anchor,
            ticket.seq,
            prepared.anchor(),
            self.cache.anchor(),
        );
        let (anchor, parent_hash, prior) = match plan {
            ApplyPlan::Refused(outcome) => return outcome,
            ApplyPlan::Admitted {
                anchor,
                parent_hash,
                prior,
            } => (anchor, parent_hash, prior),
        };
        // The generation this transition is about to seal, captured BEFORE the
        // move by the read-only lookup: the cache's installed handle, never a
        // minted one. A prior anchor the cache does not hold captures nothing —
        // the read window stays shut rather than being filled with a fresh cold
        // generation that would answer `Ready` under the parent's anchor. After
        // the seal that same handle answers every value it served at its own
        // anchor, because the parent chain moved into the sealed node the child
        // now owns.
        let prior = prior.and_then(|installed| {
            self.cache
                .installed_hub(installed)
                .map(|hub| (installed, hub))
        });
        let (_hub, report) = self
            .cache
            .apply_prepared(anchor, parent_hash, relevance, prepared);
        let report = Arc::new(report);
        state.record_settle(anchor, &report, prior);
        state.settle_current(Arc::clone(&report));
        HeadOutcome::Settled(report)
    }

    /// The whole background phase: claim this head's preparation, fetch and
    /// decode the list, and apply it — the shape a caller spawns or joins
    /// against its candidate work.
    ///
    /// Only one caller prepares a given head: a second concurrent advance for the
    /// same ticket returns [`HeadOutcome::InFlight`] instead of duplicating the
    /// fetch, and a dropped (cancelled) advance releases the claim so a later
    /// call prepares the head normally. A settled head repeats no work.
    pub async fn advance(&self, ticket: HeadTicket, relevance: &[Address]) -> HeadOutcome {
        self.advance_with_relevance(ticket, || Arc::new(RelevanceSet::from_slice(relevance)))
            .await
    }

    /// [`Self::advance`] with the head's relevance resolved by `resolve` AFTER
    /// the list is prepared and immediately BEFORE it applies.
    ///
    /// That point is the whole reason this entry exists. Apply-time relevance
    /// is a property of the SESSION, not of the head's producer, and it is read
    /// as late as the transition allows: a path registered while the list was
    /// still in flight is included in this head's seeding, and a path registered
    /// after the apply seeds from the NEXT head (reading its own unknown slots
    /// dynamically until then). `resolve` runs at most once per head, inside the
    /// single-flight claim, so a repeat delivery that loses the claim never
    /// derives anything.
    pub async fn advance_with_relevance<F>(&self, ticket: HeadTicket, resolve: F) -> HeadOutcome
    where
        F: FnOnce() -> Arc<RelevanceSet>,
    {
        let epoch = {
            let mut state = self.state.lock();
            match state.claim(ticket.anchor, ticket.seq, self.cache.anchor()) {
                ClaimStart::Claimed(epoch) => epoch,
                ClaimStart::Refused(outcome) => return outcome,
            }
        };
        let claim = PrepareClaim {
            driver: self,
            seq: ticket.seq,
            epoch,
        };
        let prepared = self.prepare(ticket).await;
        // The relevance is read HERE: after the fetch landed, before the
        // transition. Anything that registered during the fetch window is in
        // the set this head applies with.
        let relevance = resolve();
        // The claim outlives the apply: its drop publishes the abandonment
        // only when the apply settled nothing, so a cancelled preparation and
        // a settled one stay distinguishable to the generation's waiters.
        let outcome = self.apply_members(ticket, &prepared, &relevance);
        drop(claim);
        outcome
    }

    /// The snapshot `anchor` answers with right now, never blocking.
    ///
    /// Four answers, in this order:
    ///
    /// 1. the installed generation whenever that exact anchor settled — which
    ///    keeps answering after a later head was delivered;
    /// 2. the current head's cold generation while its BAL is still preparing;
    /// 3. the PREVIOUS anchor's generation, when the transition that installed
    ///    the current head linked (see [`Self::previous`] — this is the window a
    ///    fan-out whose candidates were solved one block behind the head reads
    ///    through, and it answers with the readiness that generation reported);
    /// 4. a fresh cold generation with the reason (`Superseded` /
    ///    `Undelivered`).
    ///
    /// Answers 1 and 3 are reads of a generation that already exists — the
    /// installed one from the cache's own handle, the previous one from the
    /// handle the installing transition captured — so no snapshot can install,
    /// replace, or evict a generation. Answer 4 mints a generation per call and
    /// installs nothing: a caller that wants an installed generation asks the
    /// cache for one ([`DeltaSnapshotCache::for_anchor`]).
    pub fn snapshot(&self, anchor: ReadAnchor) -> HeadView {
        let mut state = self.state.lock();
        // The installed generation answers first, and only the cache can say so:
        // the read CLONES its handle and never mints one. An anchor the cache has
        // not installed therefore cannot be answered by installing it here — the
        // read falls through to what the lifecycle holds, and answers cold when
        // that is nothing.
        let (hub, readiness) = match self.cache.installed_hub(anchor) {
            Some(hub) => (hub, state.readiness_of(anchor)),
            None => match state.lookup(anchor) {
                Lookup::Unprepared { hub } => (hub, HeadReadiness::Cold(HeadCold::Unprepared)),
                Lookup::Previous { hub, readiness } => (hub, readiness),
                Lookup::Cold { reason } => (
                    cold_generation(&self.provider, anchor, self.retention),
                    HeadReadiness::Cold(reason),
                ),
            },
        };
        state.record_view(readiness.is_ready());
        HeadView { hub, readiness }
    }

    /// The head the driver holds — settled or still preparing.
    #[must_use]
    pub fn delivered(&self) -> Option<ReadAnchor> {
        self.state.lock().delivered()
    }

    /// The generation the cache has installed, i.e. the last head that settled.
    #[must_use]
    pub fn installed(&self) -> Option<ReadAnchor> {
        self.cache.anchor()
    }

    /// The previous anchor the read window covers: the generation the installed
    /// head replaced, when the installing transition linked. `None` after a gap,
    /// the first head, or a reorg — the window is two consecutive canonical
    /// anchors or one, never a policy.
    #[must_use]
    pub fn previous(&self) -> Option<ReadAnchor> {
        self.state.lock().previous_anchor()
    }

    /// The report the driver's own apply produced for the head it holds, when it
    /// applied one. A head that settled without this driver's apply (a delivery
    /// of an already-installed anchor) has none.
    #[must_use]
    pub fn last_report(&self) -> Option<Arc<DeltaReport>> {
        self.state.lock().last_report()
    }

    /// The lifecycle's cumulative counters.
    #[must_use]
    pub fn stats(&self) -> HeadStats {
        self.state.lock().stats()
    }

    /// The terminal result the CURRENT claim attempt on `anchor` published, when
    /// the head the driver holds is that one and that attempt has published one
    /// — the completion channel behind [`Self::wait_for_head`], readable without
    /// waiting.
    ///
    /// A head claimed again after an abandonment answers with the new attempt's
    /// result once that attempt ends (`None` while it is undecided), so an
    /// abandonment an earlier attempt published stays that attempt's waiters'
    /// answer without being the head's current one. An out-of-band
    /// [`Self::apply`] that settles the head without any retry claims it answers
    /// `Settled` the same way: the settle mints a fresh private attempt epoch on
    /// the same public ticket, because the head's current claim moves to the
    /// settled truth rather than the abandonment being rewritten.
    #[must_use]
    pub fn completion(&self, anchor: ReadAnchor) -> Option<HeadCompletion> {
        self.state.lock().completion(anchor)
    }

    /// Wait until `anchor` reaches a terminal state, then answer it: the
    /// deterministic wait a same-block caller joins the head's EXISTING
    /// preparation with (see the module docs).
    ///
    /// The wait registers on the exact `(anchor, seq)` generation's CURRENT
    /// claim attempt — only while that attempt's preparation is genuinely live —
    /// and resolves on that attempt's completion notification, then RE-READS the
    /// exact anchor. Completion is attempt-scoped, so a retry that settles the
    /// same anchor never rewrites the abandonment this wait joined: the
    /// abandonment is terminal for the attempt that published it, and the
    /// retry's settle is the next attempt's result. The returned view is never
    /// [`HeadCold::Unprepared`] and never parks without a live owner: a settle
    /// answers the settled generation ([`HeadReadiness::Ready`]) or the
    /// settled-cold reason, a supersession answers [`HeadCold::Superseded`], and
    /// a generation no live preparation owns — dropped mid-flight, or delivered
    /// and never claimed — answers [`HeadCold::Abandoned`] at that exact anchor,
    /// decided once and never retroactively changed by a later retry. It fetches
    /// nothing, claims nothing, applies nothing, and holds no lock across the
    /// wait; many waiters share one attempt's one completion, and dropping one
    /// wait cancels nothing. The wait ends on those real outcomes — never on a
    /// wall clock.
    pub async fn wait_for_head(&self, anchor: ReadAnchor) -> HeadView {
        loop {
            let registered = {
                let state = self.state.lock();
                state.registration(anchor, self.cache.anchor())
            };
            if let Some(mut signal) = registered {
                // The subscription was taken under the same lock hold that
                // read the head's state, so a notification landing between the
                // two is still seen here — there is no lost-wakeup window.
                let _ = signal.changed().await;
                // The wake is this generation's end being published. An
                // abandonment is the wait's own terminal answer: it resolves
                // here rather than registering again onto the published
                // abandonment — a retry that settles later serves NEW
                // callers, never this one retroactively.
                if matches!(*signal.borrow(), Some(HeadCompletion::Abandoned)) {
                    return self.abandoned_view(anchor);
                }
                continue;
            }
            let view = self.snapshot(anchor);
            return match &view.readiness {
                // The exact head is current and unsettled with no live
                // preparation: its terminal answer is the abandoned cold
                // view, never a park.
                HeadReadiness::Cold(HeadCold::Unprepared) => HeadView {
                    hub: view.hub,
                    readiness: HeadReadiness::Cold(HeadCold::Abandoned),
                },
                _ => view,
            };
        }
    }

    /// The wait's terminal answer for a generation no live preparation owns:
    /// the exact anchor's cold generation — carrying nothing, never another
    /// head's values — with the closed `abandoned` reason.
    fn abandoned_view(&self, anchor: ReadAnchor) -> HeadView {
        let view = self.snapshot(anchor);
        let hub = if view.readiness.is_ready() {
            // A retry settled while this wait was waking. The settled
            // generation is the anchor's truth for NEW callers, but this
            // wait's answer is the abandonment it observed, and a cold answer
            // carries nothing — so it gets its own cold generation.
            cold_generation(&self.provider, anchor, self.retention)
        } else {
            view.hub
        };
        HeadView {
            hub,
            readiness: HeadReadiness::Cold(HeadCold::Abandoned),
        }
    }
}

/// The ownership of one head's preparation ATTEMPT: dropping it — a cancelled
/// advance included — releases the claim so a later call prepares that head
/// normally.
struct PrepareClaim<'a> {
    driver: &'a BalHeadDriver,
    seq: u64,
    /// The attempt this claim took. A released attempt publishes only on its own
    /// epoch's channel, so it can never write over the attempt that replaced it.
    epoch: u64,
}

impl Drop for PrepareClaim<'_> {
    fn drop(&mut self) {
        // The attempt's end is terminal for the waiters that joined it: they wake
        // to the abandoned cold answer instead of sleeping through a preparation
        // that is gone. Any terminal this attempt already published is its last
        // word and stays, so a release that no longer owns the head's live
        // attempt leaves the pose — and that attempt's result — alone.
        self.driver.state.lock().release(self.seq, self.epoch);
    }
}

/// A cold generation at `anchor`: pinned at the caller's own anchor, carrying
/// nothing, so no value can come from another head.
fn cold_generation(
    provider: &AlloyProvider,
    anchor: ReadAnchor,
    retention: Retention,
) -> Arc<DeltaHub> {
    Arc::new(DeltaHub::fresh(provider, anchor, retention))
}

/// Whether a settled head's report says its generation carries parent values.
fn settled_readiness(report: Option<&DeltaReport>) -> HeadReadiness {
    match report.map(|report| &report.outcome) {
        Some(CarryOutcome::Cold { lineage, bal }) => HeadReadiness::Cold(HeadCold::Settled {
            lineage: lineage.clone(),
            bal: bal.clone(),
        }),
        Some(CarryOutcome::Carried { .. } | CarryOutcome::AlreadyCurrent) | None => {
            HeadReadiness::Ready
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "the pose assertions and the forbidden transport fail loudly on a broken fixture"
)]
mod tests {
    use super::*;
    use crate::sim::evm2::storage_view::StorageView;
    use alloy::primitives::U256;
    use std::task::{Context, Poll};
    use std::time::Duration;

    /// One storage identity: one word of one account.
    fn slot(tag: u8, index: u64) -> (Address, U256) {
        (Address::new([tag; 20]), U256::from(index))
    }

    /// One storage word.
    fn value(word: u64) -> U256 {
        U256::from(word)
    }

    /// The delivery identity a pose carries.
    fn delivery(anchor: ReadAnchor, seq: u64) -> HeadDelivery {
        HeadDelivery {
            anchor,
            parent_hash: B256::new([0x99; 32]),
            seq,
        }
    }

    /// One transition's report, told apart from another by the identity count it
    /// reports: the tests here compare reports by identity, never by contents.
    fn report(held: usize) -> Arc<DeltaReport> {
        Arc::new(DeltaReport {
            outcome: CarryOutcome::Carried {
                carried_values: held,
            },
            writes: Vec::new(),
            writes_overwritten: 0,
            writes_skipped: 0,
            bal_latency: Duration::ZERO,
            link_time: Duration::ZERO,
            compaction: None,
            held,
            live: held,
        })
    }

    /// The settled pose ALWAYS stores the report its own settle produced, whichever
    /// pose the settle found.
    ///
    /// The report-assignment rule is the machine's, so it is pinned here, on the
    /// machine: a settle that lands on a head already carrying a report replaces it
    /// with the transition it just ran, and a delivered head settling for the first
    /// time stores its own. A head that never settles — the delivery that found its
    /// generation installed — carries [`Option::None`] and can overwrite nothing.
    #[test]
    fn a_settle_stores_the_report_it_just_produced_over_the_one_the_head_held() {
        let provider = forbidden_provider();
        let anchor = ReadAnchor::identified(11, B256::new([0xaa; 32]));
        let older = report(1);
        let newer = report(2);

        // A settled head settling again — the pose a second settle at the same head
        // finds — replaces the report it was holding.
        let mut settled_again = HeadLifecycle {
            current: Some(CurrentHead::Settled(SettledHead {
                delivery: delivery(anchor, 1),
                report: Some(Arc::clone(&older)),
                attempt: ClaimAttempt::settled(0),
            })),
            ..HeadLifecycle::default()
        };
        settled_again.settle_current(Arc::clone(&newer));
        let stored = settled_again
            .last_report()
            .expect("a settle always stores the report it produced");
        assert!(
            Arc::ptr_eq(&stored, &newer),
            "the settle replaced the report the head was holding"
        );
        assert!(
            !Arc::ptr_eq(&stored, &older),
            "the older transition's report is gone"
        );
        assert!(
            settled_again
                .current
                .as_ref()
                .and_then(CurrentHead::provisional)
                .is_none(),
            "a settled head keeps no cold generation"
        );
        assert_eq!(
            settled_again.completion(anchor),
            Some(HeadCompletion::Settled),
            "the settle published on the attempt the head already carried"
        );

        // A delivered head settling for the first time stores its own transition.
        let mut first = HeadLifecycle {
            current: Some(CurrentHead::Unclaimed(UnsettledHead {
                delivery: delivery(anchor, 2),
                provisional: cold_generation(&provider, anchor, Retention::Unbounded),
                attempt: ClaimAttempt::delivered(),
            })),
            ..HeadLifecycle::default()
        };
        first.settle_current(Arc::clone(&newer));
        let stored = first
            .last_report()
            .expect("a first settle stores the report it produced");
        assert!(
            Arc::ptr_eq(&stored, &newer),
            "a delivered head settling stores its own transition"
        );
        assert_eq!(
            first.completion(anchor),
            Some(HeadCompletion::Settled),
            "and publishes it on the attempt the delivery owned"
        );

        // The pose that runs no transition never holds a report: the delivery that
        // found its generation installed reports nothing, so it cannot overwrite
        // one.
        let installed = CurrentHead::Settled(SettledHead {
            delivery: delivery(anchor, 3),
            report: None,
            attempt: ClaimAttempt::settled(0),
        });
        assert_eq!(installed.report(), None);
    }

    /// A provider no request may reach: a snapshot answers from state already in
    /// memory, so any dispatch here is a bug and panics.
    #[derive(Debug, Clone)]
    struct ForbiddenTransport;

    impl tower::Service<alloy::rpc::json_rpc::RequestPacket> for ForbiddenTransport {
        type Response = alloy::rpc::json_rpc::ResponsePacket;
        type Error = alloy::transports::TransportError;
        type Future = alloy::transports::TransportFut<'static>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: alloy::rpc::json_rpc::RequestPacket) -> Self::Future {
            panic!("a snapshot read dispatches no request");
        }
    }

    /// The provider behind [`ForbiddenTransport`].
    fn forbidden_provider() -> AlloyProvider {
        let client =
            alloy::rpc::client::ClientBuilder::default().transport(ForbiddenTransport, true);
        let root: alloy::providers::RootProvider = alloy::providers::RootProvider::new(client);
        AlloyProvider::from_provider(Arc::new(root))
    }

    /// The cache holds ONE installed generation and the lifecycle remembers the
    /// head that settled; a read at an anchor only one of the two names answers
    /// cold instead of reconciling them by installing. The cache's anchor, the
    /// handle behind it and the values it serves stay exactly where they were,
    /// and every cold answer is its own generation.
    #[test]
    fn a_read_at_an_anchor_the_installed_generation_does_not_back_answers_cold_without_installing()
    {
        let provider = forbidden_provider();
        let (address, index) = slot(0xaa, 1);
        let installed = ReadAnchor::identified(10, B256::new([0xaa; 32]));
        let named = ReadAnchor::identified(11, B256::new([0xbb; 32]));

        let driver = BalHeadDriver::new(&provider);
        let installed_hub = driver.cache.for_anchor(installed);
        installed_hub.put(address, index, value(5));

        // The degraded pose: the head the lifecycle holds settled at an anchor
        // the cache has not installed a generation for.
        driver.state.lock().current = Some(CurrentHead::Settled(SettledHead {
            delivery: HeadDelivery {
                anchor: named,
                parent_hash: B256::new([0xcc; 32]),
                seq: 1,
            },
            report: None,
            attempt: ClaimAttempt::settled(1),
        }));

        let view = driver.snapshot(named);
        assert!(
            !view.readiness.is_ready(),
            "a generation the cache does not hold backs no readiness, got {:?}",
            view.readiness
        );
        assert_eq!(
            view.hub.snapshot_value(address, index),
            None,
            "the cold answer carries nothing"
        );
        assert_eq!(
            driver.installed(),
            Some(installed),
            "the read installed nothing and evicted nothing"
        );
        let survivor = driver
            .cache
            .installed_hub(installed)
            .expect("the installed generation survives a read it did not back");
        assert!(
            Arc::ptr_eq(&installed_hub, &survivor),
            "the same handle, not a replacement"
        );
        assert_eq!(
            survivor.snapshot_value(address, index),
            Some(value(5)),
            "and it still serves its own values"
        );

        let again = driver.snapshot(named);
        assert!(
            !Arc::ptr_eq(&view.hub, &again.hub),
            "a cold answer is minted per call, never installed"
        );
        assert_eq!(driver.installed(), Some(installed));
    }
}
